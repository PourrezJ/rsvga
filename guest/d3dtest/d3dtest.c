/*
 * d3dtest: Direct3D 11 rendering checks for rsvga, run in the guest.
 *
 * Each test draws into an offscreen target, reads the pixels back and
 * compares them with what Direct3D specifies: a colored triangle, constant
 * buffers and an indexed quad, a texture, depth, blending, instancing, an
 * integer target, scissors, SV_VertexID, multisample resolve, mipmap
 * generation, the video processor, and last a window with a swap chain (left on screen a few
 * seconds, to be seen). Results go to the host (guest/common/hostlog.h):
 * one "PASS name" or "FAIL name: why" line per test, then a summary.
 *
 * Then what Direct3D 10 and 11 add on top: depth comparison sampling,
 * gather, geometry shaders, stream output, and at feature level 11_0
 * compute shaders (typed, structured and raw UA views, group-shared
 * memory, atomics, bit instructions), UA views in pixel shaders,
 * tessellation and indirect draws and dispatches ("SKIP name" below the
 * level they need).
 *
 * Shaders are HLSL compiled at run time by d3dcompiler_47.dll (part of
 * Windows 10 and 11): shader model 4.0 for the first tests, the model each
 * later test is about for the others.
 *
 * Usage: d3dtest [seconds the window stays, default 5]
 * Build: scripts/build-guest-tools.sh (mingw-w64).
 */
#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d11.h>
#include <d3dcompiler.h>
#include <dxgi.h>
#include <stdlib.h>

#include "../common/hostlog.h"

#define W 64
#define H 64

/* R8G8B8A8_UNORM pixels read as little-endian words. */
#define RGBA(r, g, b, a) \
    ((uint32_t)(r) | (uint32_t)(g) << 8 | (uint32_t)(b) << 16 | (uint32_t)(a) << 24)
#define BLACK RGBA(0, 0, 0, 255)

static ID3D11Device *dev;
static ID3D11DeviceContext *ctx;
static pD3DCompile compile_fn;
static int passed, failed;
static const char *test_name;
static int test_failures;

/* ---- test bookkeeping -------------------------------------------------- */

static void begin(const char *name)
{
    test_name = name;
    test_failures = 0;
    ID3D11DeviceContext_ClearState(ctx);
}

static void fail(const char *fmt, ...)
{
    char line[512];
    va_list ap;

    va_start(ap, fmt);
    vsnprintf(line, sizeof line, fmt, ap);
    va_end(ap);
    say("FAIL %s: %s", test_name, line);
    test_failures++;
}

static void end(void)
{
    if (test_failures == 0) {
        say("PASS %s", test_name);
        passed++;
    } else {
        failed++;
    }
}

static int ok(const char *what, HRESULT hr)
{
    if (SUCCEEDED(hr))
        return 1;
    fail("%s: hr=0x%08lx", what, (unsigned long)hr);
    return 0;
}

/* Each 8-bit channel within `tolerance`. */
static int close_to(uint32_t got, uint32_t want, int tolerance)
{
    int i;

    for (i = 0; i < 32; i += 8) {
        int a = (int)(got >> i & 0xff), b = (int)(want >> i & 0xff);
        if (abs(a - b) > tolerance)
            return 0;
    }
    return 1;
}

static void expect(const char *what, uint32_t got, uint32_t want, int tolerance)
{
    if (!close_to(got, want, tolerance))
        fail("%s = %08x, expected %08x", what, got, want);
}

/* ---- resources --------------------------------------------------------- */

static ID3DBlob *compile(const char *src, const char *entry, const char *target)
{
    ID3DBlob *code = NULL, *errors = NULL;
    HRESULT hr = compile_fn(src, strlen(src), test_name, NULL, NULL, entry, target,
                            D3DCOMPILE_OPTIMIZATION_LEVEL3, 0, &code, &errors);

    if (FAILED(hr))
        fail("compiling %s: hr=0x%08lx %s", entry, (unsigned long)hr,
             errors ? (const char *)ID3D10Blob_GetBufferPointer(errors) : "");
    if (errors)
        ID3D10Blob_Release(errors);
    return code;
}

/* Vertex shader `vs` of `src` for `profile`, and its input layout if
 * `elements`. */
static ID3D11VertexShader *make_vs_for(const char *src, const char *profile,
                                       const D3D11_INPUT_ELEMENT_DESC *elements, UINT count,
                                       ID3D11InputLayout **layout)
{
    ID3D11VertexShader *vs = NULL;
    ID3DBlob *code = compile(src, "vs", profile);

    if (!code)
        return NULL;
    ok("CreateVertexShader",
       ID3D11Device_CreateVertexShader(dev, ID3D10Blob_GetBufferPointer(code),
                                       ID3D10Blob_GetBufferSize(code), NULL, &vs));
    if (count)
        ok("CreateInputLayout",
           ID3D11Device_CreateInputLayout(dev, elements, count,
                                          ID3D10Blob_GetBufferPointer(code),
                                          ID3D10Blob_GetBufferSize(code), layout));
    ID3D10Blob_Release(code);
    return vs;
}

static ID3D11VertexShader *make_vs(const char *src, const D3D11_INPUT_ELEMENT_DESC *elements,
                                   UINT count, ID3D11InputLayout **layout)
{
    return make_vs_for(src, "vs_4_0", elements, count, layout);
}

static ID3D11PixelShader *make_ps_for(const char *src, const char *profile)
{
    ID3D11PixelShader *ps = NULL;
    ID3DBlob *code = compile(src, "ps", profile);

    if (!code)
        return NULL;
    ok("CreatePixelShader",
       ID3D11Device_CreatePixelShader(dev, ID3D10Blob_GetBufferPointer(code),
                                      ID3D10Blob_GetBufferSize(code), NULL, &ps));
    ID3D10Blob_Release(code);
    return ps;
}

static ID3D11PixelShader *make_ps(const char *src)
{
    return make_ps_for(src, "ps_4_0");
}

static ID3D11Buffer *make_buffer(UINT bind, const void *data, UINT size)
{
    D3D11_BUFFER_DESC d = {size, D3D11_USAGE_DEFAULT, bind, 0, 0, 0};
    D3D11_SUBRESOURCE_DATA init = {data, 0, 0};
    ID3D11Buffer *b = NULL;

    ok("CreateBuffer", ID3D11Device_CreateBuffer(dev, &d, &init, &b));
    return b;
}

/* An offscreen render target and its CPU copy. */
struct target {
    ID3D11Texture2D *tex, *staging;
    ID3D11RenderTargetView *rtv;
    UINT width, height;
};

static int make_target(struct target *t, DXGI_FORMAT format, UINT width, UINT height)
{
    D3D11_TEXTURE2D_DESC d = {width, height, 1, 1, format, {1, 0}, D3D11_USAGE_DEFAULT,
                              D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE, 0, 0};

    memset(t, 0, sizeof *t);
    t->width = width;
    t->height = height;
    if (!ok("CreateTexture2D (target)", ID3D11Device_CreateTexture2D(dev, &d, NULL, &t->tex)))
        return 0;
    if (!ok("CreateRenderTargetView",
            ID3D11Device_CreateRenderTargetView(dev, (ID3D11Resource *)t->tex, NULL, &t->rtv)))
        return 0;
    d.Usage = D3D11_USAGE_STAGING;
    d.BindFlags = 0;
    d.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    return ok("CreateTexture2D (staging)",
              ID3D11Device_CreateTexture2D(dev, &d, NULL, &t->staging));
}

static void free_target(struct target *t)
{
    if (t->staging)
        ID3D11Texture2D_Release(t->staging);
    if (t->rtv)
        ID3D11RenderTargetView_Release(t->rtv);
    if (t->tex)
        ID3D11Texture2D_Release(t->tex);
}

/* Copies a 4-byte-per-pixel texture to `out` (width * height words). */
static int read_texture(ID3D11Texture2D *tex, ID3D11Texture2D *staging, UINT width,
                        UINT height, uint32_t *out)
{
    D3D11_MAPPED_SUBRESOURCE map;
    UINT y;

    ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)staging, (ID3D11Resource *)tex);
    if (!ok("Map", ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)staging, 0, D3D11_MAP_READ,
                                           0, &map)))
        return 0;
    for (y = 0; y < height; y++)
        memcpy(out + y * width, (const char *)map.pData + y * map.RowPitch, width * 4);
    ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)staging, 0);
    return 1;
}

static int read_target(struct target *t, uint32_t *out)
{
    return read_texture(t->tex, t->staging, t->width, t->height, out);
}

static void viewport(UINT width, UINT height)
{
    D3D11_VIEWPORT v = {0, 0, (float)width, (float)height, 0, 1};
    ID3D11DeviceContext_RSSetViewports(ctx, 1, &v);
}

static void bind_target(struct target *t, ID3D11DepthStencilView *dsv)
{
    ID3D11DeviceContext_OMSetRenderTargets(ctx, 1, &t->rtv, dsv);
    viewport(t->width, t->height);
}

static void clear(struct target *t, float r, float g, float b, float a)
{
    const float color[4] = {r, g, b, a};
    ID3D11DeviceContext_ClearRenderTargetView(ctx, t->rtv, color);
}

static void set_vertices(ID3D11Buffer *vb, UINT stride)
{
    UINT offset = 0;
    ID3D11DeviceContext_IASetVertexBuffers(ctx, 0, 1, &vb, &stride, &offset);
}

#define RELEASE(p)                                \
    do {                                          \
        if (p)                                    \
            IUnknown_Release((IUnknown *)(p));    \
        (p) = NULL;                               \
    } while (0)

static uint32_t pixels[W * H];
#define AT(x, y) pixels[(y) * W + (x)]

/* ---- shaders ----------------------------------------------------------- */

static const char color_hlsl[] =
    "struct V { float2 pos : POSITION; float4 color : COLOR; };\n"
    "struct P { float4 pos : SV_Position; float4 color : COLOR; };\n"
    "P vs(V v) { P p; p.pos = float4(v.pos, 0, 1); p.color = v.color; return p; }\n"
    "float4 ps(P p) : SV_Target { return p.color; }\n";

static const D3D11_INPUT_ELEMENT_DESC color_layout[] = {
    {"POSITION", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0},
    {"COLOR", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 0, 8, D3D11_INPUT_PER_VERTEX_DATA, 0},
};

/* Position (float2) then texture coordinates (float2). */
static const float full_quad[] = {
    -1, 1, 0, 0, /**/ 1, 1, 1, 0, /**/ -1, -1, 0, 1, /**/ 1, -1, 1, 1,
};
static const UINT16 quad_indices[] = {0, 1, 2, 2, 1, 3};

/* ---- tests --------------------------------------------------------------- */

/* Vertex colors interpolated over a clockwise triangle. */
static void test_triangle(void)
{
    static const float vertices[] = {
        0.0f, 0.9f, 1, 0, 0, 1,  /* top: red */
        0.9f, -0.9f, 0, 1, 0, 1, /* bottom right: green */
        -0.9f, -0.9f, 0, 0, 1, 1 /* bottom left: blue */
    };
    struct target t;
    ID3D11InputLayout *layout = NULL;
    ID3D11VertexShader *vs = NULL;
    ID3D11PixelShader *ps = NULL;
    ID3D11Buffer *vb = NULL;

    begin("triangle");
    if (!make_target(&t, DXGI_FORMAT_R8G8B8A8_UNORM, W, H))
        goto out;
    vs = make_vs(color_hlsl, color_layout, 2, &layout);
    ps = make_ps(color_hlsl);
    vb = make_buffer(D3D11_BIND_VERTEX_BUFFER, vertices, sizeof vertices);
    if (!vs || !ps || !vb || !layout)
        goto out;
    bind_target(&t, NULL);
    clear(&t, 0, 0, 0, 1);
    ID3D11DeviceContext_IASetInputLayout(ctx, layout);
    ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    set_vertices(vb, 24);
    ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
    ID3D11DeviceContext_PSSetShader(ctx, ps, NULL, 0);
    ID3D11DeviceContext_Draw(ctx, 3, 0);
    if (!read_target(&t, pixels))
        goto out;
    expect("outside (0,0)", AT(0, 0), BLACK, 0);
    expect("outside (63,10)", AT(63, 10), BLACK, 0);
    expect("near the top", AT(32, 6), RGBA(234, 11, 10, 255), 12);
    /* Barycentric weights at (32.5, 42.5): 0.318, 0.350, 0.332. */
    expect("interpolated (32,42)", AT(32, 42), RGBA(81, 89, 85, 255), 6);
out:
    RELEASE(vb);
    RELEASE(ps);
    RELEASE(vs);
    RELEASE(layout);
    free_target(&t);
    end();
}

/* One constant buffer read by both stages; a 16-bit indexed quad. */
static void test_constants(void)
{
    static const char hlsl[] =
        "cbuffer C : register(b0) { float4 color; float4 offset; };\n"
        "float4 vs(float2 pos : POSITION) : SV_Position { return float4(pos + offset.xy, 0, 1); }\n"
        "float4 ps() : SV_Target { return color; }\n";
    static const D3D11_INPUT_ELEMENT_DESC el[] = {
        {"POSITION", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0},
    };
    static const float vertices[] = {-0.5f, 0.5f, 0.5f, 0.5f, -0.5f, -0.5f, 0.5f, -0.5f};
    static const float constants[] = {0.2f, 0.4f, 0.6f, 1, 0.25f, 0.25f, 0, 0};
    struct target t;
    ID3D11InputLayout *layout = NULL;
    ID3D11VertexShader *vs = NULL;
    ID3D11PixelShader *ps = NULL;
    ID3D11Buffer *vb = NULL, *ib = NULL, *cb = NULL;

    begin("constants");
    if (!make_target(&t, DXGI_FORMAT_R8G8B8A8_UNORM, W, H))
        goto out;
    vs = make_vs(hlsl, el, 1, &layout);
    ps = make_ps(hlsl);
    vb = make_buffer(D3D11_BIND_VERTEX_BUFFER, vertices, sizeof vertices);
    ib = make_buffer(D3D11_BIND_INDEX_BUFFER, quad_indices, sizeof quad_indices);
    cb = make_buffer(D3D11_BIND_CONSTANT_BUFFER, constants, sizeof constants);
    if (!vs || !ps || !vb || !ib || !cb || !layout)
        goto out;
    bind_target(&t, NULL);
    clear(&t, 0, 0, 0, 1);
    ID3D11DeviceContext_IASetInputLayout(ctx, layout);
    ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    set_vertices(vb, 8);
    ID3D11DeviceContext_IASetIndexBuffer(ctx, ib, DXGI_FORMAT_R16_UINT, 0);
    ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
    ID3D11DeviceContext_VSSetConstantBuffers(ctx, 0, 1, &cb);
    ID3D11DeviceContext_PSSetShader(ctx, ps, NULL, 0);
    ID3D11DeviceContext_PSSetConstantBuffers(ctx, 0, 1, &cb);
    ID3D11DeviceContext_DrawIndexed(ctx, 6, 0, 0);
    if (!read_target(&t, pixels))
        goto out;
    /* The quad moved by (0.25, 0.25): pixels x 24..56, y 8..40. */
    expect("inside (40,24)", AT(40, 24), RGBA(51, 102, 153, 255), 1);
    expect("inside (25,9)", AT(25, 9), RGBA(51, 102, 153, 255), 1);
    expect("outside (10,50)", AT(10, 50), BLACK, 0);
    expect("outside (60,4)", AT(60, 4), BLACK, 0);
out:
    RELEASE(cb);
    RELEASE(ib);
    RELEASE(vb);
    RELEASE(ps);
    RELEASE(vs);
    RELEASE(layout);
    free_target(&t);
    end();
}

static const char texture_hlsl[] =
    "Texture2D t : register(t0); SamplerState s : register(s0);\n"
    "cbuffer C : register(b0) { float lod; };\n"
    "struct P { float4 pos : SV_Position; float2 uv : TEXCOORD; };\n"
    "P vs(float2 pos : POSITION, float2 uv : TEXCOORD) {\n"
    "  P p; p.pos = float4(pos, 0, 1); p.uv = uv; return p; }\n"
    "float4 ps(P p) : SV_Target { return t.SampleLevel(s, p.uv, lod); }\n";

static const D3D11_INPUT_ELEMENT_DESC texture_layout[] = {
    {"POSITION", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0},
    {"TEXCOORD", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 8, D3D11_INPUT_PER_VERTEX_DATA, 0},
};

/* Draws `srv` over the whole target, sampled at mip level `lod`. */
static void draw_textured(struct target *t, ID3D11ShaderResourceView *srv,
                          D3D11_FILTER filter, float lod)
{
    D3D11_SAMPLER_DESC sd = {filter, D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_TEXTURE_ADDRESS_CLAMP,
                             D3D11_TEXTURE_ADDRESS_CLAMP, 0, 1, D3D11_COMPARISON_NEVER,
                             {0, 0, 0, 0}, 0, D3D11_FLOAT32_MAX};
    const float constants[4] = {lod, 0, 0, 0};
    ID3D11InputLayout *layout = NULL;
    ID3D11VertexShader *vs = make_vs(texture_hlsl, texture_layout, 2, &layout);
    ID3D11PixelShader *ps = make_ps(texture_hlsl);
    ID3D11Buffer *vb = make_buffer(D3D11_BIND_VERTEX_BUFFER, full_quad, sizeof full_quad);
    ID3D11Buffer *ib = make_buffer(D3D11_BIND_INDEX_BUFFER, quad_indices, sizeof quad_indices);
    ID3D11Buffer *cb = make_buffer(D3D11_BIND_CONSTANT_BUFFER, constants, sizeof constants);
    ID3D11SamplerState *sampler = NULL;

    ok("CreateSamplerState", ID3D11Device_CreateSamplerState(dev, &sd, &sampler));
    if (vs && ps && vb && ib && cb && layout && sampler) {
        bind_target(t, NULL);
        ID3D11DeviceContext_IASetInputLayout(ctx, layout);
        ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        set_vertices(vb, 16);
        ID3D11DeviceContext_IASetIndexBuffer(ctx, ib, DXGI_FORMAT_R16_UINT, 0);
        ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
        ID3D11DeviceContext_PSSetShader(ctx, ps, NULL, 0);
        ID3D11DeviceContext_PSSetShaderResources(ctx, 0, 1, &srv);
        ID3D11DeviceContext_PSSetSamplers(ctx, 0, 1, &sampler);
        ID3D11DeviceContext_PSSetConstantBuffers(ctx, 0, 1, &cb);
        ID3D11DeviceContext_DrawIndexed(ctx, 6, 0, 0);
    }
    RELEASE(sampler);
    RELEASE(cb);
    RELEASE(ib);
    RELEASE(vb);
    RELEASE(ps);
    RELEASE(vs);
    RELEASE(layout);
}

/* A 4x4 texture of four colored quadrants, point-sampled. */
static void test_texture(void)
{
    D3D11_TEXTURE2D_DESC d = {4, 4, 1, 1, DXGI_FORMAT_R8G8B8A8_UNORM, {1, 0},
                              D3D11_USAGE_DEFAULT, D3D11_BIND_SHADER_RESOURCE, 0, 0};
    uint32_t texels[16];
    D3D11_SUBRESOURCE_DATA init = {texels, 16, 0};
    struct target t;
    ID3D11Texture2D *tex = NULL;
    ID3D11ShaderResourceView *srv = NULL;
    int x, y;

    for (y = 0; y < 4; y++)
        for (x = 0; x < 4; x++)
            texels[y * 4 + x] = y < 2 ? (x < 2 ? RGBA(255, 0, 0, 255) : RGBA(0, 255, 0, 255))
                                      : (x < 2 ? RGBA(0, 0, 255, 255) : RGBA(255, 255, 255, 255));
    begin("texture");
    if (!make_target(&t, DXGI_FORMAT_R8G8B8A8_UNORM, W, H) ||
        !ok("CreateTexture2D", ID3D11Device_CreateTexture2D(dev, &d, &init, &tex)) ||
        !ok("CreateShaderResourceView",
            ID3D11Device_CreateShaderResourceView(dev, (ID3D11Resource *)tex, NULL, &srv)))
        goto out;
    clear(&t, 0, 0, 0, 1);
    draw_textured(&t, srv, D3D11_FILTER_MIN_MAG_MIP_POINT, 0);
    if (!read_target(&t, pixels))
        goto out;
    expect("red quadrant", AT(16, 16), RGBA(255, 0, 0, 255), 0);
    expect("green quadrant", AT(48, 16), RGBA(0, 255, 0, 255), 0);
    expect("blue quadrant", AT(16, 48), RGBA(0, 0, 255, 255), 0);
    expect("white quadrant", AT(48, 48), RGBA(255, 255, 255, 255), 0);
out:
    RELEASE(srv);
    RELEASE(tex);
    free_target(&t);
    end();
}

static const char solid_hlsl[] =
    "cbuffer C : register(b0) { float4 color; float4 offset; };\n"
    "float4 vs(float3 pos : POSITION) : SV_Position { return float4(pos.xy * offset.z + offset.xy, pos.z, 1); }\n"
    "float4 ps() : SV_Target { return color; }\n";

static const D3D11_INPUT_ELEMENT_DESC solid_layout[] = {
    {"POSITION", 0, DXGI_FORMAT_R32G32B32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0},
};

/* Draws a quad of `color` at depth z: centered on (x, y), half size s. */
static void draw_solid(float z, const float color[4], float x, float y, float s)
{
    const float vertices[] = {-1, 1, z, 1, 1, z, -1, -1, z, 1, -1, z};
    const float constants[8] = {color[0], color[1], color[2], color[3], x, y, s, 0};
    ID3D11InputLayout *layout = NULL;
    ID3D11VertexShader *vs = make_vs(solid_hlsl, solid_layout, 1, &layout);
    ID3D11PixelShader *ps = make_ps(solid_hlsl);
    ID3D11Buffer *vb = make_buffer(D3D11_BIND_VERTEX_BUFFER, vertices, sizeof vertices);
    ID3D11Buffer *ib = make_buffer(D3D11_BIND_INDEX_BUFFER, quad_indices, sizeof quad_indices);
    ID3D11Buffer *cb = make_buffer(D3D11_BIND_CONSTANT_BUFFER, constants, sizeof constants);

    if (vs && ps && vb && ib && cb && layout) {
        ID3D11DeviceContext_IASetInputLayout(ctx, layout);
        ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        set_vertices(vb, 12);
        ID3D11DeviceContext_IASetIndexBuffer(ctx, ib, DXGI_FORMAT_R16_UINT, 0);
        ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
        ID3D11DeviceContext_VSSetConstantBuffers(ctx, 0, 1, &cb);
        ID3D11DeviceContext_PSSetShader(ctx, ps, NULL, 0);
        ID3D11DeviceContext_PSSetConstantBuffers(ctx, 0, 1, &cb);
        ID3D11DeviceContext_DrawIndexed(ctx, 6, 0, 0);
    }
    RELEASE(cb);
    RELEASE(ib);
    RELEASE(vb);
    RELEASE(ps);
    RELEASE(vs);
    RELEASE(layout);
}

/* Depth test against a cleared depth buffer, in a 24-bit and a 32-bit format. */
static void test_depth(DXGI_FORMAT format, const char *name)
{
    static const float red[4] = {1, 0, 0, 1}, green[4] = {0, 1, 0, 1}, blue[4] = {0, 0, 1, 1};
    D3D11_TEXTURE2D_DESC d = {W, H, 1, 1, format, {1, 0}, D3D11_USAGE_DEFAULT,
                              D3D11_BIND_DEPTH_STENCIL, 0, 0};
    struct target t;
    ID3D11Texture2D *depth = NULL;
    ID3D11DepthStencilView *dsv = NULL;

    begin(name);
    if (!make_target(&t, DXGI_FORMAT_R8G8B8A8_UNORM, W, H) ||
        !ok("CreateTexture2D (depth)", ID3D11Device_CreateTexture2D(dev, &d, NULL, &depth)) ||
        !ok("CreateDepthStencilView",
            ID3D11Device_CreateDepthStencilView(dev, (ID3D11Resource *)depth, NULL, &dsv)))
        goto out;
    clear(&t, 0, 0, 0, 1);
    ID3D11DeviceContext_ClearDepthStencilView(ctx, dsv, D3D11_CLEAR_DEPTH | D3D11_CLEAR_STENCIL,
                                              0.5f, 0);
    bind_target(&t, dsv);
    /* Default state: depth test "less", writes on. */
    draw_solid(0.25f, red, 0, 0, 1);      /* in front of 0.5: drawn */
    draw_solid(0.75f, green, 0, 0, 1);    /* behind: hidden */
    draw_solid(0.4f, blue, 0.5f, 0, 0.5f); /* behind the red (0.25): hidden */
    if (!read_target(&t, pixels))
        goto out;
    expect("front quad", AT(16, 32), RGBA(255, 0, 0, 255), 0);
    expect("behind both", AT(48, 32), RGBA(255, 0, 0, 255), 0);
out:
    RELEASE(dsv);
    RELEASE(depth);
    free_target(&t);
    end();
}

/* Alpha blending over a cleared target. */
static void test_blend(void)
{
    static const float half_red[4] = {1, 0, 0, 0.5f};
    D3D11_BLEND_DESC bd;
    struct target t;
    ID3D11BlendState *blend = NULL;

    memset(&bd, 0, sizeof bd);
    bd.RenderTarget[0].BlendEnable = TRUE;
    bd.RenderTarget[0].SrcBlend = D3D11_BLEND_SRC_ALPHA;
    bd.RenderTarget[0].DestBlend = D3D11_BLEND_INV_SRC_ALPHA;
    bd.RenderTarget[0].BlendOp = D3D11_BLEND_OP_ADD;
    bd.RenderTarget[0].SrcBlendAlpha = D3D11_BLEND_ONE;
    bd.RenderTarget[0].DestBlendAlpha = D3D11_BLEND_ZERO;
    bd.RenderTarget[0].BlendOpAlpha = D3D11_BLEND_OP_ADD;
    bd.RenderTarget[0].RenderTargetWriteMask = D3D11_COLOR_WRITE_ENABLE_ALL;
    begin("blend");
    if (!make_target(&t, DXGI_FORMAT_R8G8B8A8_UNORM, W, H) ||
        !ok("CreateBlendState", ID3D11Device_CreateBlendState(dev, &bd, &blend)))
        goto out;
    clear(&t, 0, 0, 1, 1);
    bind_target(&t, NULL);
    ID3D11DeviceContext_OMSetBlendState(ctx, blend, NULL, 0xffffffff);
    draw_solid(0, half_red, 0, 0, 0.5f);
    if (!read_target(&t, pixels))
        goto out;
    expect("blended", AT(32, 32), RGBA(128, 0, 128, 128), 1);
    expect("untouched", AT(2, 2), RGBA(0, 0, 255, 255), 0);
out:
    RELEASE(blend);
    free_target(&t);
    end();
}

/* Four instances of a quad: offsets and colors per instance, slot 1. */
static void test_instancing(void)
{
    static const char hlsl[] =
        "struct P { float4 pos : SV_Position; float4 color : COLOR; };\n"
        "P vs(float2 pos : POSITION, float2 offset : OFFSET, float4 color : COLOR) {\n"
        "  P p; p.pos = float4(pos * 0.25 + offset, 0, 1); p.color = color; return p; }\n"
        "float4 ps(P p) : SV_Target { return p.color; }\n";
    static const D3D11_INPUT_ELEMENT_DESC el[] = {
        {"POSITION", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0},
        {"OFFSET", 0, DXGI_FORMAT_R32G32_FLOAT, 1, 0, D3D11_INPUT_PER_INSTANCE_DATA, 1},
        {"COLOR", 0, DXGI_FORMAT_R8G8B8A8_UNORM, 1, 8, D3D11_INPUT_PER_INSTANCE_DATA, 1},
    };
    static const float vertices[] = {-1, 1, 1, 1, -1, -1, 1, -1};
    struct instance { float x, y; uint32_t color; } instances[4] = {
        {-0.5f, 0.5f, RGBA(255, 0, 0, 255)},
        {0.5f, 0.5f, RGBA(0, 255, 0, 255)},
        {-0.5f, -0.5f, RGBA(0, 0, 255, 255)},
        {0.5f, -0.5f, RGBA(255, 255, 0, 255)},
    };
    UINT strides[2] = {8, sizeof(struct instance)}, offsets[2] = {0, 0};
    struct target t;
    ID3D11InputLayout *layout = NULL;
    ID3D11VertexShader *vs = NULL;
    ID3D11PixelShader *ps = NULL;
    ID3D11Buffer *vbs[2] = {NULL, NULL}, *ib = NULL;

    begin("instancing");
    if (!make_target(&t, DXGI_FORMAT_R8G8B8A8_UNORM, W, H))
        goto out;
    vs = make_vs(hlsl, el, 3, &layout);
    ps = make_ps(hlsl);
    vbs[0] = make_buffer(D3D11_BIND_VERTEX_BUFFER, vertices, sizeof vertices);
    vbs[1] = make_buffer(D3D11_BIND_VERTEX_BUFFER, instances, sizeof instances);
    ib = make_buffer(D3D11_BIND_INDEX_BUFFER, quad_indices, sizeof quad_indices);
    if (!vs || !ps || !vbs[0] || !vbs[1] || !ib || !layout)
        goto out;
    bind_target(&t, NULL);
    clear(&t, 0, 0, 0, 1);
    ID3D11DeviceContext_IASetInputLayout(ctx, layout);
    ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    ID3D11DeviceContext_IASetVertexBuffers(ctx, 0, 2, vbs, strides, offsets);
    ID3D11DeviceContext_IASetIndexBuffer(ctx, ib, DXGI_FORMAT_R16_UINT, 0);
    ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
    ID3D11DeviceContext_PSSetShader(ctx, ps, NULL, 0);
    ID3D11DeviceContext_DrawIndexedInstanced(ctx, 6, 4, 0, 0, 0);
    if (!read_target(&t, pixels))
        goto out;
    expect("instance 0", AT(16, 16), RGBA(255, 0, 0, 255), 0);
    expect("instance 1", AT(48, 16), RGBA(0, 255, 0, 255), 0);
    expect("instance 2", AT(16, 48), RGBA(0, 0, 255, 255), 0);
    expect("instance 3", AT(48, 48), RGBA(255, 255, 0, 255), 0);
    expect("between", AT(32, 32), BLACK, 0);
out:
    RELEASE(ib);
    RELEASE(vbs[1]);
    RELEASE(vbs[0]);
    RELEASE(ps);
    RELEASE(vs);
    RELEASE(layout);
    free_target(&t);
    end();
}

/* An integer vertex attribute to an integer target, exact. */
static void test_integers(void)
{
    static const char hlsl[] =
        "struct P { float4 pos : SV_Position; nointerpolation uint value : VALUE; };\n"
        "P vs(float2 pos : POSITION, uint value : VALUE) {\n"
        "  P p; p.pos = float4(pos, 0, 1); p.value = value; return p; }\n"
        "uint ps(P p) : SV_Target { return p.value * 3u + 1u; }\n";
    static const D3D11_INPUT_ELEMENT_DESC el[] = {
        {"POSITION", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0},
        {"VALUE", 0, DXGI_FORMAT_R32_UINT, 0, 8, D3D11_INPUT_PER_VERTEX_DATA, 0},
    };
    struct vertex { float x, y; uint32_t value; } vertices[3] = {
        {-1, 1, 0x01234567}, {3, 1, 0x01234567}, {-1, -3, 0x01234567},
    };
    struct target t;
    ID3D11InputLayout *layout = NULL;
    ID3D11VertexShader *vs = NULL;
    ID3D11PixelShader *ps = NULL;
    ID3D11Buffer *vb = NULL;

    begin("integers");
    if (!make_target(&t, DXGI_FORMAT_R32_UINT, W, H))
        goto out;
    vs = make_vs(hlsl, el, 2, &layout);
    ps = make_ps(hlsl);
    vb = make_buffer(D3D11_BIND_VERTEX_BUFFER, vertices, sizeof vertices);
    if (!vs || !ps || !vb || !layout)
        goto out;
    bind_target(&t, NULL);
    clear(&t, 0, 0, 0, 0);
    ID3D11DeviceContext_IASetInputLayout(ctx, layout);
    ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    set_vertices(vb, sizeof(struct vertex));
    ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
    ID3D11DeviceContext_PSSetShader(ctx, ps, NULL, 0);
    ID3D11DeviceContext_Draw(ctx, 3, 0);
    if (!read_target(&t, pixels))
        goto out;
    if (AT(5, 5) != 0x01234567u * 3u + 1u || AT(60, 60) != AT(5, 5))
        fail("got %08x and %08x, expected %08x", AT(5, 5), AT(60, 60), 0x01234567u * 3u + 1u);
out:
    RELEASE(vb);
    RELEASE(ps);
    RELEASE(vs);
    RELEASE(layout);
    free_target(&t);
    end();
}

/* The scissor rectangle bounds a full-target quad. */
static void test_scissor(void)
{
    static const float white[4] = {1, 1, 1, 1};
    D3D11_RASTERIZER_DESC rd = {D3D11_FILL_SOLID, D3D11_CULL_BACK, FALSE, 0, 0, 0,
                                TRUE, TRUE, FALSE, FALSE};
    D3D11_RECT rect = {16, 8, 48, 40};
    struct target t;
    ID3D11RasterizerState *rs = NULL;

    begin("scissor");
    if (!make_target(&t, DXGI_FORMAT_R8G8B8A8_UNORM, W, H) ||
        !ok("CreateRasterizerState", ID3D11Device_CreateRasterizerState(dev, &rd, &rs)))
        goto out;
    clear(&t, 0, 0, 0, 1);
    bind_target(&t, NULL);
    ID3D11DeviceContext_RSSetState(ctx, rs);
    ID3D11DeviceContext_RSSetScissorRects(ctx, 1, &rect);
    draw_solid(0, white, 0, 0, 1);
    if (!read_target(&t, pixels))
        goto out;
    expect("inside", AT(32, 20), RGBA(255, 255, 255, 255), 0);
    expect("inside corner", AT(16, 8), RGBA(255, 255, 255, 255), 0);
    expect("left", AT(15, 20), BLACK, 0);
    expect("below", AT(32, 40), BLACK, 0);
out:
    RELEASE(rs);
    free_target(&t);
    end();
}

/* No vertex buffer: positions from SV_VertexID. */
static void test_vertex_id(void)
{
    static const char hlsl[] =
        "float4 vs(uint id : SV_VertexID) : SV_Position {\n"
        "  float2 uv = float2((id << 1) & 2, id & 2);\n"
        "  return float4(uv * float2(2, -2) + float2(-1, 1), 0, 1); }\n"
        "float4 ps() : SV_Target { return float4(1, 1, 0, 1); }\n";
    struct target t;
    ID3D11VertexShader *vs = NULL;
    ID3D11PixelShader *ps = NULL;

    begin("vertex id");
    if (!make_target(&t, DXGI_FORMAT_R8G8B8A8_UNORM, W, H))
        goto out;
    vs = make_vs(hlsl, NULL, 0, NULL);
    ps = make_ps(hlsl);
    if (!vs || !ps)
        goto out;
    bind_target(&t, NULL);
    clear(&t, 0, 0, 0, 1);
    ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
    ID3D11DeviceContext_PSSetShader(ctx, ps, NULL, 0);
    ID3D11DeviceContext_Draw(ctx, 3, 0);
    if (!read_target(&t, pixels))
        goto out;
    expect("top left", AT(1, 1), RGBA(255, 255, 0, 255), 0);
    expect("bottom right", AT(62, 62), RGBA(255, 255, 0, 255), 0);
out:
    RELEASE(ps);
    RELEASE(vs);
    free_target(&t);
    end();
}

/* A triangle drawn 4x multisampled, resolved. */
static void test_msaa(void)
{
    static const float vertices[] = {
        -1, 1, 1, 0, 1, 1, /**/ 1, 1, 1, 0, 1, 1, /**/ -1, -1, 1, 0, 1, 1,
    };
    D3D11_TEXTURE2D_DESC d = {W, H, 1, 1, DXGI_FORMAT_R8G8B8A8_UNORM, {4, 0},
                              D3D11_USAGE_DEFAULT, D3D11_BIND_RENDER_TARGET, 0, 0};
    struct target t, ms;
    ID3D11InputLayout *layout = NULL;
    ID3D11VertexShader *vs = NULL;
    ID3D11PixelShader *ps = NULL;
    ID3D11Buffer *vb = NULL;
    UINT quality = 0;

    begin("msaa resolve");
    memset(&ms, 0, sizeof ms);
    if (!make_target(&t, DXGI_FORMAT_R8G8B8A8_UNORM, W, H))
        goto out;
    ID3D11Device_CheckMultisampleQualityLevels(dev, DXGI_FORMAT_R8G8B8A8_UNORM, 4, &quality);
    if (quality == 0) {
        fail("4x multisampling is not offered");
        goto out;
    }
    ms.width = W;
    ms.height = H;
    if (!ok("CreateTexture2D (4x)", ID3D11Device_CreateTexture2D(dev, &d, NULL, &ms.tex)) ||
        !ok("CreateRenderTargetView (4x)",
            ID3D11Device_CreateRenderTargetView(dev, (ID3D11Resource *)ms.tex, NULL, &ms.rtv)))
        goto out;
    vs = make_vs(color_hlsl, color_layout, 2, &layout);
    ps = make_ps(color_hlsl);
    vb = make_buffer(D3D11_BIND_VERTEX_BUFFER, vertices, sizeof vertices);
    if (!vs || !ps || !vb || !layout)
        goto out;
    bind_target(&ms, NULL);
    clear(&ms, 0, 0, 0, 1);
    ID3D11DeviceContext_IASetInputLayout(ctx, layout);
    ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    set_vertices(vb, 24);
    ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
    ID3D11DeviceContext_PSSetShader(ctx, ps, NULL, 0);
    ID3D11DeviceContext_Draw(ctx, 3, 0);
    ID3D11DeviceContext_ResolveSubresource(ctx, (ID3D11Resource *)t.tex, 0,
                                           (ID3D11Resource *)ms.tex, 0,
                                           DXGI_FORMAT_R8G8B8A8_UNORM);
    if (!read_target(&t, pixels))
        goto out;
    expect("covered", AT(10, 10), RGBA(255, 0, 255, 255), 0);
    expect("uncovered", AT(54, 54), BLACK, 0);
    /* On the diagonal, some samples only: a mix. */
    if (AT(31, 32) == RGBA(255, 0, 255, 255) || AT(31, 32) == BLACK)
        say("note %s: edge pixel %08x is not a mix", test_name, AT(31, 32));
out:
    RELEASE(vb);
    RELEASE(ps);
    RELEASE(vs);
    RELEASE(layout);
    if (ms.rtv)
        ID3D11RenderTargetView_Release(ms.rtv);
    if (ms.tex)
        ID3D11Texture2D_Release(ms.tex);
    free_target(&t);
    end();
}

/* GenerateMips on a checkerboard: the 1x1 level is mid-gray. */
static void test_mips(void)
{
    D3D11_TEXTURE2D_DESC d = {8, 8, 4, 1, DXGI_FORMAT_R8G8B8A8_UNORM, {1, 0},
                              D3D11_USAGE_DEFAULT,
                              D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET, 0,
                              D3D11_RESOURCE_MISC_GENERATE_MIPS};
    uint32_t texels[64];
    struct target t;
    ID3D11Texture2D *tex = NULL;
    ID3D11ShaderResourceView *srv = NULL;
    int i;

    for (i = 0; i < 64; i++)
        texels[i] = ((i % 8) + (i / 8)) % 2 ? RGBA(255, 255, 255, 255) : BLACK;
    begin("generate mips");
    if (!make_target(&t, DXGI_FORMAT_R8G8B8A8_UNORM, W, H) ||
        !ok("CreateTexture2D", ID3D11Device_CreateTexture2D(dev, &d, NULL, &tex)) ||
        !ok("CreateShaderResourceView",
            ID3D11Device_CreateShaderResourceView(dev, (ID3D11Resource *)tex, NULL, &srv)))
        goto out;
    ID3D11DeviceContext_UpdateSubresource(ctx, (ID3D11Resource *)tex, 0, NULL, texels, 32, 0);
    ID3D11DeviceContext_GenerateMips(ctx, srv);
    clear(&t, 0, 0, 0, 1);
    draw_textured(&t, srv, D3D11_FILTER_MIN_MAG_MIP_POINT, 3);
    if (!read_target(&t, pixels))
        goto out;
    expect("level 3", AT(32, 32), RGBA(128, 128, 128, 255), 3);
    clear(&t, 0, 0, 0, 1);
    draw_textured(&t, srv, D3D11_FILTER_MIN_MAG_MIP_POINT, 0);
    if (!read_target(&t, pixels))
        goto out;
    expect("level 0 kept", AT(4, 4), BLACK, 0);
    expect("level 0 kept", AT(12, 4), RGBA(255, 255, 255, 255), 0);
out:
    RELEASE(srv);
    RELEASE(tex);
    free_target(&t);
    end();
}

/* ---- video processing ------------------------------------------------------ */

/* B8G8R8A8 pixels read as little-endian words. */
#define BGRA(r, g, b, a) RGBA(b, g, r, a)

/*
 * The video processor, which players and DWM use to scale and convert
 * video: a 32x32 picture, left half red and right half blue, scaled into a
 * 64x64 target. VMware's driver runs WDDM 2.0 only on a device that
 * announces video blits; this shows what it sends for them. Skipped (not
 * failed) when the driver offers no video device.
 */
static void test_video(void)
{
    D3D11_VIDEO_PROCESSOR_CONTENT_DESC content = {
        D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, {30, 1}, 32, 32, {30, 1}, 64, 64,
        D3D11_VIDEO_USAGE_PLAYBACK_NORMAL};
    D3D11_TEXTURE2D_DESC d = {32, 32, 1, 1, DXGI_FORMAT_B8G8R8A8_UNORM, {1, 0},
                              D3D11_USAGE_DEFAULT,
                              D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE, 0, 0};
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC in_desc = {0, D3D11_VPIV_DIMENSION_TEXTURE2D,
                                                     .Texture2D = {0, 0}};
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC out_desc = {D3D11_VPOV_DIMENSION_TEXTURE2D,
                                                       .Texture2D = {0}};
    D3D11_VIDEO_PROCESSOR_STREAM stream;
    ID3D11VideoDevice *vdev = NULL;
    ID3D11VideoContext *vctx = NULL;
    ID3D11VideoProcessorEnumerator *vpe = NULL;
    ID3D11VideoProcessor *vp = NULL;
    ID3D11VideoProcessorInputView *in_view = NULL;
    ID3D11VideoProcessorOutputView *out_view = NULL;
    ID3D11Texture2D *src = NULL;
    uint32_t texels[32 * 32];
    struct target t;
    UINT support = 0;
    HRESULT hr;
    int i;

    memset(&t, 0, sizeof t);
    hr = ID3D11Device_QueryInterface(dev, &IID_ID3D11VideoDevice, (void **)&vdev);
    if (FAILED(hr)) {
        say("SKIP video: no video device (hr=0x%08lx)", (unsigned long)hr);
        return;
    }
    begin("video");
    for (i = 0; i < 32 * 32; i++)
        texels[i] = i % 32 < 16 ? BGRA(255, 0, 0, 255) : BGRA(0, 0, 255, 255);
    if (!ok("QueryInterface (video context)",
            ID3D11DeviceContext_QueryInterface(ctx, &IID_ID3D11VideoContext, (void **)&vctx)) ||
        !ok("CreateVideoProcessorEnumerator",
            ID3D11VideoDevice_CreateVideoProcessorEnumerator(vdev, &content, &vpe)))
        goto out;
    ID3D11VideoProcessorEnumerator_CheckVideoProcessorFormat(vpe, DXGI_FORMAT_B8G8R8A8_UNORM,
                                                              &support);
    say("video: B8G8R8A8 support 0x%x", support);
    if (!ok("CreateVideoProcessor", ID3D11VideoDevice_CreateVideoProcessor(vdev, vpe, 0, &vp)) ||
        !ok("CreateTexture2D (video)", ID3D11Device_CreateTexture2D(dev, &d, NULL, &src)) ||
        !make_target(&t, DXGI_FORMAT_B8G8R8A8_UNORM, W, H))
        goto out;
    ID3D11DeviceContext_UpdateSubresource(ctx, (ID3D11Resource *)src, 0, NULL, texels, 32 * 4, 0);
    if (!ok("CreateVideoProcessorInputView",
            ID3D11VideoDevice_CreateVideoProcessorInputView(vdev, (ID3D11Resource *)src, vpe,
                                                            &in_desc, &in_view)) ||
        !ok("CreateVideoProcessorOutputView",
            ID3D11VideoDevice_CreateVideoProcessorOutputView(vdev, (ID3D11Resource *)t.tex, vpe,
                                                             &out_desc, &out_view)))
        goto out;
    clear(&t, 0, 0, 0, 1);
    memset(&stream, 0, sizeof stream);
    stream.Enable = TRUE;
    stream.pInputSurface = in_view;
    if (!ok("VideoProcessorBlt", ID3D11VideoContext_VideoProcessorBlt(vctx, vp, out_view, 0, 1,
                                                                       &stream)) ||
        !read_target(&t, pixels))
        goto out;
    expect("left half", AT(16, 32), BGRA(255, 0, 0, 255), 8);
    expect("right half", AT(48, 32), BGRA(0, 0, 255, 255), 8);
out:
    RELEASE(out_view);
    RELEASE(in_view);
    RELEASE(src);
    RELEASE(vp);
    RELEASE(vpe);
    RELEASE(vctx);
    RELEASE(vdev);
    free_target(&t);
    end();
}

/* ---- Direct3D 11: shader model 5 and the stages around it ------------------ */

static D3D_FEATURE_LEVEL feature_level;

/* Whether the device has `level`; else the test is skipped. */
static int needs(D3D_FEATURE_LEVEL level, const char *name)
{
    if (feature_level >= level)
        return 1;
    say("SKIP %s: feature level %x", name, (unsigned)feature_level);
    return 0;
}

/* Compiles `entry` of `src` for `profile` and creates the shader with
 * `create`, a Create*Shader method of the device. */
#define MAKE_SHADER(type, method, src, entry, profile)                                          \
    ({                                                                                          \
        type *shader_ = NULL;                                                                   \
        ID3DBlob *code_ = compile(src, entry, profile);                                         \
        if (code_) {                                                                            \
            ok(#method, ID3D11Device_##method(dev, ID3D10Blob_GetBufferPointer(code_),          \
                                              ID3D10Blob_GetBufferSize(code_), NULL, &shader_)); \
            ID3D10Blob_Release(code_);                                                          \
        }                                                                                       \
        shader_;                                                                                \
    })
#define MAKE_CS(src) MAKE_SHADER(ID3D11ComputeShader, CreateComputeShader, src, "cs", "cs_5_0")
#define MAKE_GS(src, profile) \
    MAKE_SHADER(ID3D11GeometryShader, CreateGeometryShader, src, "gs", profile)
#define MAKE_HS(src) MAKE_SHADER(ID3D11HullShader, CreateHullShader, src, "hs", "hs_5_0")
#define MAKE_DS(src) MAKE_SHADER(ID3D11DomainShader, CreateDomainShader, src, "ds", "ds_5_0")

/* A buffer of `size` bytes (`data`: initial contents, or zeros). */
static ID3D11Buffer *make_buffer_ex(UINT bind, UINT misc, UINT stride, const void *data,
                                    UINT size)
{
    D3D11_BUFFER_DESC d = {size, D3D11_USAGE_DEFAULT, bind, 0, misc, stride};
    D3D11_SUBRESOURCE_DATA init = {data, 0, 0};
    ID3D11Buffer *b = NULL;
    void *zeros = data ? NULL : calloc(1, size);

    if (!data)
        init.pSysMem = zeros;
    ok("CreateBuffer", ID3D11Device_CreateBuffer(dev, &d, &init, &b));
    free(zeros);
    return b;
}

/* Copies the `size` bytes of buffer `b` to `out`. */
static int read_buffer(ID3D11Buffer *b, UINT size, void *out)
{
    D3D11_BUFFER_DESC d = {size, D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_READ, 0, 0};
    D3D11_MAPPED_SUBRESOURCE map;
    ID3D11Buffer *staging = NULL;
    int done = 0;

    if (!ok("CreateBuffer (staging)", ID3D11Device_CreateBuffer(dev, &d, NULL, &staging)))
        return 0;
    ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)staging, (ID3D11Resource *)b);
    if (ok("Map", ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)staging, 0, D3D11_MAP_READ, 0,
                                          &map))) {
        memcpy(out, map.pData, size);
        ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)staging, 0);
        done = 1;
    }
    RELEASE(staging);
    return done;
}

/* An unordered-access view of `count` elements of buffer `b`. */
static ID3D11UnorderedAccessView *buffer_uav(ID3D11Buffer *b, DXGI_FORMAT format, UINT count,
                                             UINT flags)
{
    D3D11_UNORDERED_ACCESS_VIEW_DESC d;
    ID3D11UnorderedAccessView *v = NULL;

    memset(&d, 0, sizeof d);
    d.Format = format;
    d.ViewDimension = D3D11_UAV_DIMENSION_BUFFER;
    d.Buffer.NumElements = count;
    d.Buffer.Flags = flags;
    ok("CreateUnorderedAccessView",
       ID3D11Device_CreateUnorderedAccessView(dev, (ID3D11Resource *)b, &d, &v));
    return v;
}

/* A shader resource view of `count` elements of buffer `b` (raw: bytes
 * as 32-bit words). */
static ID3D11ShaderResourceView *buffer_srv(ID3D11Buffer *b, UINT count, int raw)
{
    D3D11_SHADER_RESOURCE_VIEW_DESC d;
    ID3D11ShaderResourceView *v = NULL;

    memset(&d, 0, sizeof d);
    d.Format = raw ? DXGI_FORMAT_R32_TYPELESS : DXGI_FORMAT_UNKNOWN;
    d.ViewDimension = D3D11_SRV_DIMENSION_BUFFEREX;
    d.BufferEx.NumElements = count;
    d.BufferEx.Flags = raw ? D3D11_BUFFEREX_SRV_FLAG_RAW : 0;
    ok("CreateShaderResourceView",
       ID3D11Device_CreateShaderResourceView(dev, (ID3D11Resource *)b, &d, &v));
    return v;
}

static void unbind_compute(void)
{
    ID3D11UnorderedAccessView *none_uav[2] = {NULL, NULL};
    ID3D11ShaderResourceView *none_srv[2] = {NULL, NULL};

    ID3D11DeviceContext_CSSetUnorderedAccessViews(ctx, 0, 2, none_uav, NULL);
    ID3D11DeviceContext_CSSetShaderResources(ctx, 0, 2, none_srv);
}

/* A compute shader writes every texel of a texture (typed UA view). */
static void test_compute(void)
{
    static const char hlsl[] =
        "RWTexture2D<unorm float4> u : register(u0);\n"
        "[numthreads(8, 8, 1)]\n"
        "void cs(uint3 id : SV_DispatchThreadID) {\n"
        "  u[id.xy] = float4(id.x / 63.0, id.y / 63.0, 0.5, 1); }\n";
    D3D11_TEXTURE2D_DESC d = {W, H, 1, 1, DXGI_FORMAT_R8G8B8A8_UNORM, {1, 0}, D3D11_USAGE_DEFAULT,
                              D3D11_BIND_UNORDERED_ACCESS | D3D11_BIND_SHADER_RESOURCE, 0, 0};
    ID3D11Texture2D *tex = NULL, *staging = NULL;
    ID3D11UnorderedAccessView *uav = NULL;
    ID3D11ComputeShader *cs = NULL;

    if (!needs(D3D_FEATURE_LEVEL_11_0, "compute"))
        return;
    begin("compute");
    cs = MAKE_CS(hlsl);
    if (!cs || !ok("CreateTexture2D", ID3D11Device_CreateTexture2D(dev, &d, NULL, &tex)) ||
        !ok("CreateUnorderedAccessView",
            ID3D11Device_CreateUnorderedAccessView(dev, (ID3D11Resource *)tex, NULL, &uav)))
        goto out;
    d.Usage = D3D11_USAGE_STAGING;
    d.BindFlags = 0;
    d.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    if (!ok("CreateTexture2D (staging)", ID3D11Device_CreateTexture2D(dev, &d, NULL, &staging)))
        goto out;
    ID3D11DeviceContext_CSSetShader(ctx, cs, NULL, 0);
    ID3D11DeviceContext_CSSetUnorderedAccessViews(ctx, 0, 1, &uav, NULL);
    ID3D11DeviceContext_Dispatch(ctx, W / 8, H / 8, 1);
    unbind_compute();
    if (!read_texture(tex, staging, W, H, pixels))
        goto out;
    expect("(0,0)", AT(0, 0), RGBA(0, 0, 128, 255), 1);
    expect("(63,0)", AT(63, 0), RGBA(255, 0, 128, 255), 1);
    expect("(10,40)", AT(10, 40), RGBA(40, 162, 128, 255), 1);
    expect("(63,63)", AT(63, 63), RGBA(255, 255, 128, 255), 1);
out:
    RELEASE(cs);
    RELEASE(uav);
    RELEASE(staging);
    RELEASE(tex);
    end();
}

/* Structured buffers: one read (shader resource), one written. */
static void test_structured(void)
{
    static const char hlsl[] =
        "struct In { uint a; uint b; };\n"
        "StructuredBuffer<In> input : register(t0);\n"
        "RWStructuredBuffer<uint> output : register(u0);\n"
        "[numthreads(64, 1, 1)]\n"
        "void cs(uint3 id : SV_DispatchThreadID) {\n"
        "  In v = input[id.x]; output[id.x] = v.a * v.b + id.x; }\n";
    uint32_t in[256 * 2], out[256];
    ID3D11Buffer *input = NULL, *output = NULL;
    ID3D11ShaderResourceView *srv = NULL;
    ID3D11UnorderedAccessView *uav = NULL;
    ID3D11ComputeShader *cs = NULL;
    int i;

    if (!needs(D3D_FEATURE_LEVEL_11_0, "structured buffers"))
        return;
    begin("structured buffers");
    for (i = 0; i < 256; i++) {
        in[2 * i] = i;
        in[2 * i + 1] = 3;
    }
    cs = MAKE_CS(hlsl);
    input = make_buffer_ex(D3D11_BIND_SHADER_RESOURCE, D3D11_RESOURCE_MISC_BUFFER_STRUCTURED, 8,
                           in, sizeof in);
    output = make_buffer_ex(D3D11_BIND_UNORDERED_ACCESS, D3D11_RESOURCE_MISC_BUFFER_STRUCTURED, 4,
                            NULL, sizeof out);
    if (!cs || !input || !output)
        goto out;
    srv = buffer_srv(input, 256, 0);
    uav = buffer_uav(output, DXGI_FORMAT_UNKNOWN, 256, 0);
    if (!srv || !uav)
        goto out;
    ID3D11DeviceContext_CSSetShader(ctx, cs, NULL, 0);
    ID3D11DeviceContext_CSSetShaderResources(ctx, 0, 1, &srv);
    ID3D11DeviceContext_CSSetUnorderedAccessViews(ctx, 0, 1, &uav, NULL);
    ID3D11DeviceContext_Dispatch(ctx, 4, 1, 1);
    unbind_compute();
    if (!read_buffer(output, sizeof out, out))
        goto out;
    for (i = 0; i < 256; i++)
        if (out[i] != 4u * i) {
            fail("element %d = %u, expected %u", i, out[i], 4u * i);
            break;
        }
out:
    RELEASE(cs);
    RELEASE(uav);
    RELEASE(srv);
    RELEASE(output);
    RELEASE(input);
    end();
}

/* Raw (byte address) buffers and group-shared memory with a barrier: each
 * group reverses its 64 words. */
static void test_groupshared(void)
{
    static const char hlsl[] =
        "ByteAddressBuffer input : register(t0);\n"
        "RWByteAddressBuffer output : register(u0);\n"
        "groupshared uint words[64];\n"
        "[numthreads(64, 1, 1)]\n"
        "void cs(uint3 gid : SV_GroupID, uint gi : SV_GroupIndex) {\n"
        "  words[gi] = input.Load((gid.x * 64 + gi) * 4);\n"
        "  GroupMemoryBarrierWithGroupSync();\n"
        "  output.Store((gid.x * 64 + gi) * 4, words[63 - gi]); }\n";
    uint32_t in[128], out[128];
    ID3D11Buffer *input = NULL, *output = NULL;
    ID3D11ShaderResourceView *srv = NULL;
    ID3D11UnorderedAccessView *uav = NULL;
    ID3D11ComputeShader *cs = NULL;
    int i;

    if (!needs(D3D_FEATURE_LEVEL_11_0, "group shared"))
        return;
    begin("group shared");
    for (i = 0; i < 128; i++)
        in[i] = 0x1000u + i * 7u;
    cs = MAKE_CS(hlsl);
    input = make_buffer_ex(D3D11_BIND_SHADER_RESOURCE, D3D11_RESOURCE_MISC_BUFFER_ALLOW_RAW_VIEWS,
                           0, in, sizeof in);
    output = make_buffer_ex(D3D11_BIND_UNORDERED_ACCESS, D3D11_RESOURCE_MISC_BUFFER_ALLOW_RAW_VIEWS,
                            0, NULL, sizeof out);
    if (!cs || !input || !output)
        goto out;
    srv = buffer_srv(input, 128, 1);
    uav = buffer_uav(output, DXGI_FORMAT_R32_TYPELESS, 128, D3D11_BUFFER_UAV_FLAG_RAW);
    if (!srv || !uav)
        goto out;
    ID3D11DeviceContext_CSSetShader(ctx, cs, NULL, 0);
    ID3D11DeviceContext_CSSetShaderResources(ctx, 0, 1, &srv);
    ID3D11DeviceContext_CSSetUnorderedAccessViews(ctx, 0, 1, &uav, NULL);
    ID3D11DeviceContext_Dispatch(ctx, 2, 1, 1);
    unbind_compute();
    if (!read_buffer(output, sizeof out, out))
        goto out;
    for (i = 0; i < 128; i++) {
        uint32_t want = in[(i / 64) * 64 + 63 - i % 64];
        if (out[i] != want) {
            fail("word %d = %08x, expected %08x", i, out[i], want);
            break;
        }
    }
out:
    RELEASE(cs);
    RELEASE(uav);
    RELEASE(srv);
    RELEASE(output);
    RELEASE(input);
    end();
}

/* Atomic operations, on group-shared memory and on a buffer. */
static void test_atomics(void)
{
    static const char hlsl[] =
        "RWByteAddressBuffer counters : register(u0);\n"
        "groupshared uint sum;\n"
        "[numthreads(64, 1, 1)]\n"
        "void cs(uint3 gid : SV_GroupID, uint gi : SV_GroupIndex) {\n"
        "  if (gi == 0) sum = 0;\n"
        "  GroupMemoryBarrierWithGroupSync();\n"
        "  InterlockedAdd(sum, gi + 1);\n"
        "  GroupMemoryBarrierWithGroupSync();\n"
        "  uint old;\n"
        "  if (gi == 0) { counters.InterlockedAdd(0, sum, old); counters.InterlockedMax(4, gid.x); }\n"
        "  counters.InterlockedAdd(8, 1, old);\n"
        "  counters.InterlockedOr(12, 1u << (gid.x + gi % 2)); }\n";
    uint32_t out[4];
    ID3D11Buffer *buffer = NULL;
    ID3D11UnorderedAccessView *uav = NULL;
    ID3D11ComputeShader *cs = NULL;

    if (!needs(D3D_FEATURE_LEVEL_11_0, "atomics"))
        return;
    begin("atomics");
    cs = MAKE_CS(hlsl);
    buffer = make_buffer_ex(D3D11_BIND_UNORDERED_ACCESS, D3D11_RESOURCE_MISC_BUFFER_ALLOW_RAW_VIEWS,
                            0, NULL, sizeof out);
    if (!cs || !buffer)
        goto out;
    uav = buffer_uav(buffer, DXGI_FORMAT_R32_TYPELESS, 4, D3D11_BUFFER_UAV_FLAG_RAW);
    if (!uav)
        goto out;
    ID3D11DeviceContext_CSSetShader(ctx, cs, NULL, 0);
    ID3D11DeviceContext_CSSetUnorderedAccessViews(ctx, 0, 1, &uav, NULL);
    ID3D11DeviceContext_Dispatch(ctx, 10, 1, 1);
    unbind_compute();
    if (!read_buffer(buffer, sizeof out, out))
        goto out;
    if (out[0] != 10 * 2080 || out[1] != 9 || out[2] != 640 || out[3] != 0x7ff)
        fail("got %u, %u, %u, %x; expected 20800, 9, 640, 7ff", out[0], out[1], out[2], out[3]);
out:
    RELEASE(cs);
    RELEASE(uav);
    RELEASE(buffer);
    end();
}

/* Shader model 5 bit instructions, half floats and reciprocals. */
static void test_bits(void)
{
    static const char hlsl[] =
        "cbuffer C : register(b0) { uint4 v; };\n"
        "RWStructuredBuffer<uint> o : register(u0);\n"
        "[numthreads(1, 1, 1)]\n"
        "void cs() {\n"
        "  o[0] = countbits(v.x);\n"
        "  o[1] = firstbithigh(v.x);\n"
        "  o[2] = firstbitlow(v.x);\n"
        "  o[3] = reversebits(v.x);\n"
        "  o[4] = f32tof16(asfloat(v.y));\n"
        "  o[5] = asuint(f16tof32(v.z));\n"
        "  o[6] = (v.x >> 4) & 0xff;\n"
        "  o[7] = asuint(rcp(asfloat(v.y)));\n"
        "  o[8] = firstbithigh(int(v.w));\n"
        "  o[9] = asuint(int(v.w) >> 8); }\n";
    static const uint32_t constants[4] = {0x00f0f010, 0x3fc00000 /* 1.5 */, 0x3c00 /* 1.0 */,
                                          0xffff0000};
    static const uint32_t want[10] = {9, 23, 4, 0x080f0f00, 0x3e00, 0x3f800000, 0x01,
                                      0x3f2aaaab, 15, 0xffffff00};
    uint32_t out[10];
    ID3D11Buffer *cb = NULL, *buffer = NULL;
    ID3D11UnorderedAccessView *uav = NULL;
    ID3D11ComputeShader *cs = NULL;
    int i;

    if (!needs(D3D_FEATURE_LEVEL_11_0, "bit instructions"))
        return;
    begin("bit instructions");
    cs = MAKE_CS(hlsl);
    cb = make_buffer(D3D11_BIND_CONSTANT_BUFFER, constants, sizeof constants);
    buffer = make_buffer_ex(D3D11_BIND_UNORDERED_ACCESS, D3D11_RESOURCE_MISC_BUFFER_STRUCTURED, 4,
                            NULL, sizeof out);
    if (!cs || !cb || !buffer)
        goto out;
    uav = buffer_uav(buffer, DXGI_FORMAT_UNKNOWN, 10, 0);
    if (!uav)
        goto out;
    ID3D11DeviceContext_CSSetShader(ctx, cs, NULL, 0);
    ID3D11DeviceContext_CSSetConstantBuffers(ctx, 0, 1, &cb);
    ID3D11DeviceContext_CSSetUnorderedAccessViews(ctx, 0, 1, &uav, NULL);
    ID3D11DeviceContext_Dispatch(ctx, 1, 1, 1);
    unbind_compute();
    if (!read_buffer(buffer, sizeof out, out))
        goto out;
    for (i = 0; i < 10; i++) {
        /* rcp: within one unit in the last place. */
        int close = i == 7 ? out[i] + 1 >= want[i] && out[i] <= want[i] + 1 : out[i] == want[i];
        if (!close)
            fail("result %d = %08x, expected %08x", i, out[i], want[i]);
    }
out:
    RELEASE(cs);
    RELEASE(uav);
    RELEASE(buffer);
    RELEASE(cb);
    end();
}

/* A pixel shader counts its invocations in a UA view bound with the
 * render target. */
static void test_pixel_uav(void)
{
    static const char hlsl[] =
        "RWByteAddressBuffer counter : register(u1);\n"
        "float4 vs(float2 pos : POSITION) : SV_Position { return float4(pos, 0, 1); }\n"
        "float4 ps(float4 pos : SV_Position) : SV_Target {\n"
        "  uint old; counter.InterlockedAdd(0, 1, old); return float4(1, 0, 0, 1); }\n";
    static const D3D11_INPUT_ELEMENT_DESC el[] = {
        {"POSITION", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0},
    };
    uint32_t counts[4] = {0, 0, 0, 0};
    struct target t;
    ID3D11InputLayout *layout = NULL;
    ID3D11VertexShader *vs = NULL;
    ID3D11PixelShader *ps = NULL;
    ID3D11Buffer *vb = NULL, *ib = NULL, *buffer = NULL;
    ID3D11UnorderedAccessView *uav = NULL;

    memset(&t, 0, sizeof t);
    if (!needs(D3D_FEATURE_LEVEL_11_0, "pixel UA view"))
        return;
    begin("pixel UA view");
    if (!make_target(&t, DXGI_FORMAT_R8G8B8A8_UNORM, W, H))
        goto out;
    vs = make_vs_for(hlsl, "vs_5_0", el, 1, &layout);
    ps = make_ps_for(hlsl, "ps_5_0");
    vb = make_buffer(D3D11_BIND_VERTEX_BUFFER, full_quad, sizeof full_quad);
    ib = make_buffer(D3D11_BIND_INDEX_BUFFER, quad_indices, sizeof quad_indices);
    buffer = make_buffer_ex(D3D11_BIND_UNORDERED_ACCESS, D3D11_RESOURCE_MISC_BUFFER_ALLOW_RAW_VIEWS,
                            0, NULL, 16);
    if (!vs || !ps || !vb || !ib || !layout || !buffer)
        goto out;
    uav = buffer_uav(buffer, DXGI_FORMAT_R32_TYPELESS, 4, D3D11_BUFFER_UAV_FLAG_RAW);
    if (!uav)
        goto out;
    clear(&t, 0, 0, 0, 1);
    ID3D11DeviceContext_OMSetRenderTargetsAndUnorderedAccessViews(ctx, 1, &t.rtv, NULL, 1, 1, &uav,
                                                                  NULL);
    viewport(W, H);
    ID3D11DeviceContext_IASetInputLayout(ctx, layout);
    ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    set_vertices(vb, 16);
    ID3D11DeviceContext_IASetIndexBuffer(ctx, ib, DXGI_FORMAT_R16_UINT, 0);
    ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
    ID3D11DeviceContext_PSSetShader(ctx, ps, NULL, 0);
    ID3D11DeviceContext_DrawIndexed(ctx, 6, 0, 0);
    ID3D11DeviceContext_OMSetRenderTargets(ctx, 0, NULL, NULL);
    if (!read_buffer(buffer, sizeof counts, counts) || !read_target(&t, pixels))
        goto out;
    if (counts[0] != W * H)
        fail("%u pixel shader invocations, expected %u", counts[0], W * H);
    expect("target", AT(5, 5), RGBA(255, 0, 0, 255), 0);
out:
    RELEASE(uav);
    RELEASE(buffer);
    RELEASE(ib);
    RELEASE(vb);
    RELEASE(ps);
    RELEASE(vs);
    RELEASE(layout);
    free_target(&t);
    end();
}

/* A geometry shader makes a square of each point (shader model 4: every
 * feature level of Direct3D 10 has them). */
static void test_geometry(void)
{
    static const char hlsl[] =
        "float4 vs(float2 pos : POSITION) : SV_Position { return float4(pos, 0, 1); }\n"
        "struct G { float4 pos : SV_Position; float4 color : COLOR; };\n"
        "[maxvertexcount(4)]\n"
        "void gs(point float4 p[1] : SV_Position, inout TriangleStream<G> s) {\n"
        "  G g; g.color = float4(0, 1, 0, 1);\n"
        "  g.pos = p[0] + float4(-0.5, 0.5, 0, 0); s.Append(g);\n"
        "  g.pos = p[0] + float4(0.5, 0.5, 0, 0); s.Append(g);\n"
        "  g.pos = p[0] + float4(-0.5, -0.5, 0, 0); s.Append(g);\n"
        "  g.pos = p[0] + float4(0.5, -0.5, 0, 0); s.Append(g); }\n"
        "float4 ps(G g) : SV_Target { return g.color; }\n";
    static const D3D11_INPUT_ELEMENT_DESC el[] = {
        {"POSITION", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0},
    };
    static const float point[2] = {0, 0};
    struct target t;
    ID3D11InputLayout *layout = NULL;
    ID3D11VertexShader *vs = NULL;
    ID3D11GeometryShader *gs = NULL;
    ID3D11PixelShader *ps = NULL;
    ID3D11Buffer *vb = NULL;

    begin("geometry shader");
    if (!make_target(&t, DXGI_FORMAT_R8G8B8A8_UNORM, W, H))
        goto out;
    vs = make_vs(hlsl, el, 1, &layout);
    gs = MAKE_GS(hlsl, "gs_4_0");
    ps = make_ps(hlsl);
    vb = make_buffer(D3D11_BIND_VERTEX_BUFFER, point, sizeof point);
    if (!vs || !gs || !ps || !vb || !layout)
        goto out;
    bind_target(&t, NULL);
    clear(&t, 0, 0, 0, 1);
    ID3D11DeviceContext_IASetInputLayout(ctx, layout);
    ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_POINTLIST);
    set_vertices(vb, 8);
    ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
    ID3D11DeviceContext_GSSetShader(ctx, gs, NULL, 0);
    ID3D11DeviceContext_PSSetShader(ctx, ps, NULL, 0);
    ID3D11DeviceContext_Draw(ctx, 1, 0);
    if (!read_target(&t, pixels))
        goto out;
    expect("center", AT(32, 32), RGBA(0, 255, 0, 255), 0);
    expect("inside corner", AT(17, 46), RGBA(0, 255, 0, 255), 0);
    expect("outside", AT(8, 8), BLACK, 0);
    expect("outside right", AT(50, 32), BLACK, 0);
out:
    RELEASE(vb);
    RELEASE(ps);
    RELEASE(gs);
    RELEASE(vs);
    RELEASE(layout);
    free_target(&t);
    end();
}

/* Tessellation of a quad patch: the domain shader colors by u * v, which
 * only a finely tessellated quad shows (two triangles would be linear). */
static void test_tessellation(void)
{
    static const char hlsl[] =
        "struct C { float2 pos : POS; };\n"
        "C vs(float2 pos : POSITION) { C c; c.pos = pos; return c; }\n"
        "struct T { float edges[4] : SV_TessFactor; float inside[2] : SV_InsideTessFactor; };\n"
        "T constants(InputPatch<C, 4> p) {\n"
        "  T t; t.edges[0] = t.edges[1] = t.edges[2] = t.edges[3] = 16;\n"
        "  t.inside[0] = t.inside[1] = 16; return t; }\n"
        "[domain(\"quad\")] [partitioning(\"integer\")] [outputtopology(\"triangle_cw\")]\n"
        "[outputcontrolpoints(4)] [patchconstantfunc(\"constants\")]\n"
        "C hs(InputPatch<C, 4> p, uint i : SV_OutputControlPointID) { return p[i]; }\n"
        "struct D { float4 pos : SV_Position; float4 color : COLOR; };\n"
        "[domain(\"quad\")]\n"
        "D ds(T t, float2 uv : SV_DomainLocation, const OutputPatch<C, 4> p) {\n"
        "  D d; float2 top = lerp(p[0].pos, p[1].pos, uv.x);\n"
        "  float2 bottom = lerp(p[2].pos, p[3].pos, uv.x);\n"
        "  d.pos = float4(lerp(top, bottom, uv.y), 0, 1);\n"
        "  d.color = float4(uv.x * uv.y, 0, 0, 1); return d; }\n"
        "float4 ps(D d) : SV_Target { return d.color; }\n";
    static const D3D11_INPUT_ELEMENT_DESC el[] = {
        {"POSITION", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0},
    };
    struct target t;
    ID3D11InputLayout *layout = NULL;
    ID3D11VertexShader *vs = NULL;
    ID3D11HullShader *hs = NULL;
    ID3D11DomainShader *ds = NULL;
    ID3D11PixelShader *ps = NULL;
    ID3D11Buffer *vb = NULL;

    memset(&t, 0, sizeof t);
    if (!needs(D3D_FEATURE_LEVEL_11_0, "tessellation"))
        return;
    begin("tessellation");
    if (!make_target(&t, DXGI_FORMAT_R8G8B8A8_UNORM, W, H))
        goto out;
    vs = make_vs_for(hlsl, "vs_5_0", el, 1, &layout);
    hs = MAKE_HS(hlsl);
    ds = MAKE_DS(hlsl);
    ps = make_ps_for(hlsl, "ps_5_0");
    vb = make_buffer(D3D11_BIND_VERTEX_BUFFER, full_quad, sizeof full_quad);
    if (!vs || !hs || !ds || !ps || !vb || !layout)
        goto out;
    bind_target(&t, NULL);
    clear(&t, 0, 0, 1, 1);
    ID3D11DeviceContext_IASetInputLayout(ctx, layout);
    ID3D11DeviceContext_IASetPrimitiveTopology(ctx,
                                               D3D11_PRIMITIVE_TOPOLOGY_4_CONTROL_POINT_PATCHLIST);
    set_vertices(vb, 16);
    ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
    ID3D11DeviceContext_HSSetShader(ctx, hs, NULL, 0);
    ID3D11DeviceContext_DSSetShader(ctx, ds, NULL, 0);
    ID3D11DeviceContext_PSSetShader(ctx, ps, NULL, 0);
    ID3D11DeviceContext_Draw(ctx, 4, 0);
    if (!read_target(&t, pixels))
        goto out;
    /* u * v at the pixel centers: (32.5/64)^2 and 48.5/64 * 16.5/64. */
    expect("center", AT(32, 32), RGBA(66, 0, 0, 255), 4);
    expect("upper right", AT(48, 16), RGBA(50, 0, 0, 255), 4);
    expect("bottom right", AT(63, 63), RGBA(251, 0, 0, 255), 4);
out:
    RELEASE(vb);
    RELEASE(ps);
    RELEASE(ds);
    RELEASE(hs);
    RELEASE(vs);
    RELEASE(layout);
    free_target(&t);
    end();
}

/* Stream output of a vertex shader (through a geometry shader made from
 * its code). Stream 0 is also rasterized (feature level 10 cannot turn
 * that off), with no pixel shader nor target: nothing is drawn. */
static void test_stream_output(void)
{
    static const char hlsl[] =
        "struct O { float4 pos : SV_Position; float2 value : VALUE; };\n"
        "O vs(float2 pos : POSITION, uint id : SV_VertexID) {\n"
        "  O o; o.pos = float4(pos, 0, 1); o.value = float2(id * 2, pos.x); return o; }\n";
    static const D3D11_INPUT_ELEMENT_DESC el[] = {
        {"POSITION", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0},
    };
    static const D3D11_SO_DECLARATION_ENTRY decl[] = {{0, "VALUE", 0, 0, 2, 0}};
    static const float points[6] = {0.25f, 0, 0.5f, 0, -1, 0};
    UINT stride = 8, offset = 0;
    float out[6];
    ID3D11InputLayout *layout = NULL;
    ID3D11VertexShader *vs = NULL;
    ID3D11GeometryShader *gs = NULL;
    ID3D11Buffer *vb = NULL, *so = NULL;
    ID3DBlob *code = NULL;

    begin("stream output");
    vs = make_vs(hlsl, el, 1, &layout);
    code = compile(hlsl, "vs", "vs_4_0");
    vb = make_buffer(D3D11_BIND_VERTEX_BUFFER, points, sizeof points);
    so = make_buffer_ex(D3D11_BIND_STREAM_OUTPUT, 0, 0, NULL, sizeof out);
    if (!vs || !code || !vb || !so || !layout ||
        !ok("CreateGeometryShaderWithStreamOutput",
            ID3D11Device_CreateGeometryShaderWithStreamOutput(
                dev, ID3D10Blob_GetBufferPointer(code), ID3D10Blob_GetBufferSize(code), decl, 1,
                &stride, 1, 0, NULL, &gs)))
        goto out;
    ID3D11DeviceContext_IASetInputLayout(ctx, layout);
    ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_POINTLIST);
    set_vertices(vb, 8);
    ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
    ID3D11DeviceContext_GSSetShader(ctx, gs, NULL, 0);
    ID3D11DeviceContext_SOSetTargets(ctx, 1, &so, &offset);
    ID3D11DeviceContext_Draw(ctx, 3, 0);
    ID3D11DeviceContext_SOSetTargets(ctx, 0, NULL, NULL);
    if (!read_buffer(so, sizeof out, out))
        goto out;
    if (out[0] != 0 || out[1] != 0.25f || out[2] != 2 || out[3] != 0.5f || out[4] != 4 ||
        out[5] != -1)
        fail("got (%g, %g) (%g, %g) (%g, %g)", out[0], out[1], out[2], out[3], out[4], out[5]);
out:
    if (code)
        ID3D10Blob_Release(code);
    RELEASE(so);
    RELEASE(vb);
    RELEASE(gs);
    RELEASE(vs);
    RELEASE(layout);
    end();
}

/* A draw and a dispatch whose arguments come from buffers. */
static void test_indirect(void)
{
    static const char cs_hlsl[] =
        "RWByteAddressBuffer counter : register(u0);\n"
        "[numthreads(4, 1, 1)]\n"
        "void cs() { uint old; counter.InterlockedAdd(0, 1, old); }\n";
    static const float vertices[] = {
        0.0f, 0.9f, 1, 0, 0, 1, 0.9f, -0.9f, 0, 1, 0, 1, -0.9f, -0.9f, 0, 0, 1, 1,
    };
    static const uint32_t draw_args[4] = {3, 1, 0, 0};
    static const uint32_t dispatch_args[3] = {3, 2, 1};
    uint32_t count = 0;
    struct target t;
    ID3D11InputLayout *layout = NULL;
    ID3D11VertexShader *vs = NULL;
    ID3D11PixelShader *ps = NULL;
    ID3D11ComputeShader *cs = NULL;
    ID3D11Buffer *vb = NULL, *args = NULL, *dargs = NULL, *counter = NULL;
    ID3D11UnorderedAccessView *uav = NULL;

    memset(&t, 0, sizeof t);
    if (!needs(D3D_FEATURE_LEVEL_11_0, "indirect"))
        return;
    begin("indirect");
    if (!make_target(&t, DXGI_FORMAT_R8G8B8A8_UNORM, W, H))
        goto out;
    vs = make_vs(color_hlsl, color_layout, 2, &layout);
    ps = make_ps(color_hlsl);
    cs = MAKE_CS(cs_hlsl);
    vb = make_buffer(D3D11_BIND_VERTEX_BUFFER, vertices, sizeof vertices);
    args = make_buffer_ex(0, D3D11_RESOURCE_MISC_DRAWINDIRECT_ARGS, 0, draw_args, sizeof draw_args);
    dargs = make_buffer_ex(0, D3D11_RESOURCE_MISC_DRAWINDIRECT_ARGS, 0, dispatch_args,
                           sizeof dispatch_args);
    counter = make_buffer_ex(D3D11_BIND_UNORDERED_ACCESS,
                             D3D11_RESOURCE_MISC_BUFFER_ALLOW_RAW_VIEWS, 0, NULL, 4);
    if (!vs || !ps || !cs || !vb || !layout || !args || !dargs || !counter)
        goto out;
    uav = buffer_uav(counter, DXGI_FORMAT_R32_TYPELESS, 1, D3D11_BUFFER_UAV_FLAG_RAW);
    if (!uav)
        goto out;
    bind_target(&t, NULL);
    clear(&t, 0, 0, 0, 1);
    ID3D11DeviceContext_IASetInputLayout(ctx, layout);
    ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    set_vertices(vb, 24);
    ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
    ID3D11DeviceContext_PSSetShader(ctx, ps, NULL, 0);
    ID3D11DeviceContext_DrawInstancedIndirect(ctx, args, 0);
    ID3D11DeviceContext_CSSetShader(ctx, cs, NULL, 0);
    ID3D11DeviceContext_CSSetUnorderedAccessViews(ctx, 0, 1, &uav, NULL);
    ID3D11DeviceContext_DispatchIndirect(ctx, dargs, 0);
    unbind_compute();
    if (!read_target(&t, pixels) || !read_buffer(counter, 4, &count))
        goto out;
    expect("triangle", AT(32, 6), RGBA(234, 11, 10, 255), 12);
    expect("outside", AT(0, 0), BLACK, 0);
    if (count != 3 * 2 * 4)
        fail("%u compute threads, expected 24", count);
out:
    RELEASE(uav);
    RELEASE(counter);
    RELEASE(dargs);
    RELEASE(args);
    RELEASE(vb);
    RELEASE(cs);
    RELEASE(ps);
    RELEASE(vs);
    RELEASE(layout);
    free_target(&t);
    end();
}

/* Depth comparison sampling (shadow maps): left half compares 0.25,
 * right half 0.75, with a depth of 0.5. */
static void test_shadow(void)
{
    static const char hlsl[] =
        "Texture2D<float> depth : register(t0);\n"
        "SamplerComparisonState cmp : register(s0);\n"
        "float4 vs(float2 pos : POSITION) : SV_Position { return float4(pos, 0, 1); }\n"
        "float4 ps(float4 pos : SV_Position) : SV_Target {\n"
        "  float lit = depth.SampleCmpLevelZero(cmp, pos.xy / 64, pos.x < 32 ? 0.25 : 0.75);\n"
        "  return float4(lit, lit, lit, 1); }\n";
    static const D3D11_INPUT_ELEMENT_DESC el[] = {
        {"POSITION", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0},
    };
    D3D11_TEXTURE2D_DESC d = {W, H, 1, 1, DXGI_FORMAT_R32_TYPELESS, {1, 0}, D3D11_USAGE_DEFAULT,
                              D3D11_BIND_DEPTH_STENCIL | D3D11_BIND_SHADER_RESOURCE, 0, 0};
    D3D11_DEPTH_STENCIL_VIEW_DESC dd = {DXGI_FORMAT_D32_FLOAT, D3D11_DSV_DIMENSION_TEXTURE2D, 0,
                                        .Texture2D = {0}};
    D3D11_SHADER_RESOURCE_VIEW_DESC sd = {DXGI_FORMAT_R32_FLOAT, D3D11_SRV_DIMENSION_TEXTURE2D,
                                          .Texture2D = {0, 1}};
    D3D11_SAMPLER_DESC smp = {D3D11_FILTER_COMPARISON_MIN_MAG_MIP_POINT,
                              D3D11_TEXTURE_ADDRESS_CLAMP,
                              D3D11_TEXTURE_ADDRESS_CLAMP,
                              D3D11_TEXTURE_ADDRESS_CLAMP,
                              0,
                              1,
                              D3D11_COMPARISON_LESS,
                              {0, 0, 0, 0},
                              0,
                              D3D11_FLOAT32_MAX};
    struct target t;
    ID3D11Texture2D *tex = NULL;
    ID3D11DepthStencilView *dsv = NULL;
    ID3D11ShaderResourceView *srv = NULL;
    ID3D11SamplerState *sampler = NULL;
    ID3D11InputLayout *layout = NULL;
    ID3D11VertexShader *vs = NULL;
    ID3D11PixelShader *ps = NULL;
    ID3D11Buffer *vb = NULL, *ib = NULL;

    begin("shadow compare");
    if (!make_target(&t, DXGI_FORMAT_R8G8B8A8_UNORM, W, H))
        goto out;
    vs = make_vs(hlsl, el, 1, &layout);
    ps = make_ps(hlsl);
    vb = make_buffer(D3D11_BIND_VERTEX_BUFFER, full_quad, sizeof full_quad);
    ib = make_buffer(D3D11_BIND_INDEX_BUFFER, quad_indices, sizeof quad_indices);
    if (!vs || !ps || !vb || !ib || !layout ||
        !ok("CreateTexture2D (depth)", ID3D11Device_CreateTexture2D(dev, &d, NULL, &tex)) ||
        !ok("CreateDepthStencilView",
            ID3D11Device_CreateDepthStencilView(dev, (ID3D11Resource *)tex, &dd, &dsv)) ||
        !ok("CreateShaderResourceView",
            ID3D11Device_CreateShaderResourceView(dev, (ID3D11Resource *)tex, &sd, &srv)) ||
        !ok("CreateSamplerState", ID3D11Device_CreateSamplerState(dev, &smp, &sampler)))
        goto out;
    ID3D11DeviceContext_ClearDepthStencilView(ctx, dsv, D3D11_CLEAR_DEPTH, 0.5f, 0);
    bind_target(&t, NULL);
    clear(&t, 0, 0, 1, 1);
    ID3D11DeviceContext_IASetInputLayout(ctx, layout);
    ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    set_vertices(vb, 16);
    ID3D11DeviceContext_IASetIndexBuffer(ctx, ib, DXGI_FORMAT_R16_UINT, 0);
    ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
    ID3D11DeviceContext_PSSetShader(ctx, ps, NULL, 0);
    ID3D11DeviceContext_PSSetShaderResources(ctx, 0, 1, &srv);
    ID3D11DeviceContext_PSSetSamplers(ctx, 0, 1, &sampler);
    ID3D11DeviceContext_DrawIndexed(ctx, 6, 0, 0);
    if (!read_target(&t, pixels))
        goto out;
    expect("in front (left)", AT(10, 32), RGBA(255, 255, 255, 255), 0);
    expect("behind (right)", AT(50, 32), BLACK, 0);
out:
    RELEASE(ib);
    RELEASE(vb);
    RELEASE(ps);
    RELEASE(vs);
    RELEASE(layout);
    RELEASE(sampler);
    RELEASE(srv);
    RELEASE(dsv);
    RELEASE(tex);
    free_target(&t);
    end();
}

/* Gather (shader model 4.1): the four texels around a point, in the order
 * Direct3D specifies. */
static void test_gather(void)
{
    static const char hlsl[] =
        "Texture2D<float> tex : register(t0);\n"
        "SamplerState s : register(s0);\n"
        "float4 vs(float2 pos : POSITION) : SV_Position { return float4(pos, 0, 1); }\n"
        "float4 ps() : SV_Target { return tex.Gather(s, float2(0.5, 0.5)); }\n";
    static const D3D11_INPUT_ELEMENT_DESC el[] = {
        {"POSITION", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0},
    };
    /* (0,0), (1,0), (0,1), (1,1). */
    static const uint8_t texels[4] = {26, 51, 77, 102};
    D3D11_TEXTURE2D_DESC d = {2, 2, 1, 1, DXGI_FORMAT_R8_UNORM, {1, 0}, D3D11_USAGE_DEFAULT,
                              D3D11_BIND_SHADER_RESOURCE, 0, 0};
    D3D11_SUBRESOURCE_DATA init = {texels, 2, 0};
    D3D11_SAMPLER_DESC smp = {D3D11_FILTER_MIN_MAG_MIP_POINT,
                              D3D11_TEXTURE_ADDRESS_CLAMP,
                              D3D11_TEXTURE_ADDRESS_CLAMP,
                              D3D11_TEXTURE_ADDRESS_CLAMP,
                              0,
                              1,
                              D3D11_COMPARISON_NEVER,
                              {0, 0, 0, 0},
                              0,
                              D3D11_FLOAT32_MAX};
    struct target t;
    ID3D11Texture2D *tex = NULL;
    ID3D11ShaderResourceView *srv = NULL;
    ID3D11SamplerState *sampler = NULL;
    ID3D11InputLayout *layout = NULL;
    ID3D11VertexShader *vs = NULL;
    ID3D11PixelShader *ps = NULL;
    ID3D11Buffer *vb = NULL, *ib = NULL;

    memset(&t, 0, sizeof t);
    if (!needs(D3D_FEATURE_LEVEL_10_1, "gather"))
        return;
    begin("gather");
    if (!make_target(&t, DXGI_FORMAT_R8G8B8A8_UNORM, W, H))
        goto out;
    vs = make_vs_for(hlsl, "vs_4_1", el, 1, &layout);
    ps = make_ps_for(hlsl, "ps_4_1");
    vb = make_buffer(D3D11_BIND_VERTEX_BUFFER, full_quad, sizeof full_quad);
    ib = make_buffer(D3D11_BIND_INDEX_BUFFER, quad_indices, sizeof quad_indices);
    if (!vs || !ps || !vb || !ib || !layout ||
        !ok("CreateTexture2D", ID3D11Device_CreateTexture2D(dev, &d, &init, &tex)) ||
        !ok("CreateShaderResourceView",
            ID3D11Device_CreateShaderResourceView(dev, (ID3D11Resource *)tex, NULL, &srv)) ||
        !ok("CreateSamplerState", ID3D11Device_CreateSamplerState(dev, &smp, &sampler)))
        goto out;
    bind_target(&t, NULL);
    clear(&t, 0, 0, 0, 1);
    ID3D11DeviceContext_IASetInputLayout(ctx, layout);
    ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    set_vertices(vb, 16);
    ID3D11DeviceContext_IASetIndexBuffer(ctx, ib, DXGI_FORMAT_R16_UINT, 0);
    ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
    ID3D11DeviceContext_PSSetShader(ctx, ps, NULL, 0);
    ID3D11DeviceContext_PSSetShaderResources(ctx, 0, 1, &srv);
    ID3D11DeviceContext_PSSetSamplers(ctx, 0, 1, &sampler);
    ID3D11DeviceContext_DrawIndexed(ctx, 6, 0, 0);
    if (!read_target(&t, pixels))
        goto out;
    /* x: (0,1), y: (1,1), z: (1,0), w: (0,0). */
    expect("gathered", AT(20, 20), RGBA(77, 102, 51, 26), 1);
out:
    RELEASE(ib);
    RELEASE(vb);
    RELEASE(ps);
    RELEASE(vs);
    RELEASE(layout);
    RELEASE(sampler);
    RELEASE(srv);
    RELEASE(tex);
    free_target(&t);
    end();
}

/* ---- the window ----------------------------------------------------------- */

static LRESULT CALLBACK window_proc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp)
{
    if (msg == WM_CLOSE) {
        DestroyWindow(hwnd);
        return 0;
    }
    return DefWindowProcA(hwnd, msg, wp, lp);
}

/* The triangle in a window, presented every frame for `seconds`. */
static void test_window(int seconds)
{
    static const float vertices[] = {
        0.0f, 0.8f, 1, 0, 0, 1, 0.8f, -0.8f, 0, 1, 0, 1, -0.8f, -0.8f, 0, 0, 1, 1,
    };
    WNDCLASSA wc;
    DXGI_SWAP_CHAIN_DESC sd;
    IDXGIDevice *dxgi_dev = NULL;
    IDXGIAdapter *adapter = NULL;
    IDXGIFactory *factory = NULL;
    IDXGISwapChain *chain = NULL;
    ID3D11Texture2D *back = NULL, *staging = NULL;
    ID3D11RenderTargetView *rtv = NULL;
    ID3D11InputLayout *layout = NULL;
    ID3D11VertexShader *vs = NULL;
    ID3D11PixelShader *ps = NULL;
    ID3D11Buffer *vb = NULL;
    D3D11_TEXTURE2D_DESC td;
    HWND hwnd;
    DWORD start;
    int frames = 0, checked = 0;
    static uint32_t window_pixels[320 * 240];

    begin("window");
    memset(&wc, 0, sizeof wc);
    wc.lpfnWndProc = window_proc;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.lpszClassName = "d3dtest";
    wc.hCursor = LoadCursor(NULL, IDC_ARROW);
    RegisterClassA(&wc);
    hwnd = CreateWindowA("d3dtest", "rsvga d3dtest", WS_OVERLAPPEDWINDOW | WS_VISIBLE, 200, 150,
                         400, 330, NULL, NULL, wc.hInstance, NULL);
    if (!hwnd) {
        fail("CreateWindow: error %lu", GetLastError());
        goto out;
    }
    memset(&sd, 0, sizeof sd);
    sd.BufferDesc.Width = 320;
    sd.BufferDesc.Height = 240;
    sd.BufferDesc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
    sd.SampleDesc.Count = 1;
    sd.BufferUsage = DXGI_USAGE_RENDER_TARGET_OUTPUT;
    sd.BufferCount = 2;
    sd.OutputWindow = hwnd;
    sd.Windowed = TRUE;
    sd.SwapEffect = DXGI_SWAP_EFFECT_FLIP_DISCARD;
    if (!ok("QueryInterface IDXGIDevice",
            ID3D11Device_QueryInterface(dev, &IID_IDXGIDevice, (void **)&dxgi_dev)) ||
        !ok("GetAdapter", IDXGIDevice_GetAdapter(dxgi_dev, &adapter)) ||
        !ok("GetParent IDXGIFactory",
            IDXGIAdapter_GetParent(adapter, &IID_IDXGIFactory, (void **)&factory)) ||
        !ok("CreateSwapChain", IDXGIFactory_CreateSwapChain(factory, (IUnknown *)dev, &sd, &chain)) ||
        !ok("GetBuffer", IDXGISwapChain_GetBuffer(chain, 0, &IID_ID3D11Texture2D, (void **)&back)) ||
        !ok("CreateRenderTargetView",
            ID3D11Device_CreateRenderTargetView(dev, (ID3D11Resource *)back, NULL, &rtv)))
        goto out;
    ID3D11Texture2D_GetDesc(back, &td);
    td.Usage = D3D11_USAGE_STAGING;
    td.BindFlags = 0;
    td.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    td.MiscFlags = 0;
    if (!ok("CreateTexture2D (staging)", ID3D11Device_CreateTexture2D(dev, &td, NULL, &staging)))
        goto out;
    vs = make_vs(color_hlsl, color_layout, 2, &layout);
    ps = make_ps(color_hlsl);
    vb = make_buffer(D3D11_BIND_VERTEX_BUFFER, vertices, sizeof vertices);
    if (!vs || !ps || !vb || !layout)
        goto out;
    start = GetTickCount();
    while (IsWindow(hwnd) && GetTickCount() - start < (DWORD)seconds * 1000) {
        const float gray[4] = {0.1f, 0.1f, 0.15f, 1};
        MSG msg;
        HRESULT hr;

        while (PeekMessageA(&msg, NULL, 0, 0, PM_REMOVE)) {
            TranslateMessage(&msg);
            DispatchMessageA(&msg);
        }
        ID3D11DeviceContext_OMSetRenderTargets(ctx, 1, &rtv, NULL);
        viewport(320, 240);
        ID3D11DeviceContext_ClearRenderTargetView(ctx, rtv, gray);
        ID3D11DeviceContext_IASetInputLayout(ctx, layout);
        ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        set_vertices(vb, 24);
        ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
        ID3D11DeviceContext_PSSetShader(ctx, ps, NULL, 0);
        ID3D11DeviceContext_Draw(ctx, 3, 0);
        if (!checked) {
            /* The back buffer before its first present: B, G, R, A bytes. */
            checked = 1;
            if (read_texture(back, staging, 320, 240, window_pixels)) {
                expect("background", window_pixels[5 * 320 + 5], 0xff1a1a26, 1);
                expect("top of the triangle", window_pixels[40 * 320 + 160], 0xffff0000, 30);
            }
            ID3D11DeviceContext_OMSetRenderTargets(ctx, 1, &rtv, NULL);
        }
        hr = IDXGISwapChain_Present(chain, 1, 0);
        if (FAILED(hr)) {
            fail("Present: hr=0x%08lx after %d frames", (unsigned long)hr, frames);
            break;
        }
        frames++;
    }
    say("note window: %d frames in %d s", frames, seconds);
out:
    RELEASE(vb);
    RELEASE(ps);
    RELEASE(vs);
    RELEASE(layout);
    RELEASE(staging);
    RELEASE(rtv);
    RELEASE(back);
    RELEASE(chain);
    RELEASE(factory);
    RELEASE(adapter);
    RELEASE(dxgi_dev);
    if (hwnd && IsWindow(hwnd))
        DestroyWindow(hwnd);
    end();
}

/* ---- main ------------------------------------------------------------------ */

int main(int argc, char **argv)
{
    static const D3D_FEATURE_LEVEL levels[] = {
        D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_10_0,
    };
    int seconds = argc > 1 ? atoi(argv[1]) : 5;
    D3D_FEATURE_LEVEL level = 0;
    HMODULE compiler;
    HRESULT hr;

    log_open("d3dtest");
    compiler = LoadLibraryA("d3dcompiler_47.dll");
    compile_fn = compiler ? (pD3DCompile)(void *)GetProcAddress(compiler, "D3DCompile") : NULL;
    if (!compile_fn) {
        say("FAIL setup: no d3dcompiler_47.dll (error %lu)", GetLastError());
        log_close();
        return 2;
    }
    hr = D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                           levels, sizeof levels / sizeof levels[0], D3D11_SDK_VERSION, &dev,
                           &level, &ctx);
    if (FAILED(hr)) {
        say("FAIL setup: D3D11CreateDevice hr=0x%08lx", (unsigned long)hr);
        log_close();
        return 2;
    }
    feature_level = level;
    say("start: feature level %x", (unsigned)level);
    test_triangle();
    test_constants();
    test_texture();
    test_depth(DXGI_FORMAT_D24_UNORM_S8_UINT, "depth 24");
    test_depth(DXGI_FORMAT_D32_FLOAT, "depth 32");
    test_blend();
    test_instancing();
    test_integers();
    test_scissor();
    test_vertex_id();
    test_msaa();
    test_mips();
    test_video();
    test_shadow();
    test_gather();
    test_geometry();
    test_stream_output();
    test_compute();
    test_structured();
    test_groupshared();
    test_atomics();
    test_bits();
    test_pixel_uav();
    test_tessellation();
    test_indirect();
    if (seconds > 0)
        test_window(seconds);
    say("device removed reason: 0x%08lx", (unsigned long)ID3D11Device_GetDeviceRemovedReason(dev));
    say("summary: %d passed, %d failed", passed, failed);
    ID3D11DeviceContext_Release(ctx);
    ID3D11Device_Release(dev);
    log_close();
    return failed ? 1 : 0;
}
