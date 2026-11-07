/*
 * d3dtest: Direct3D 11 rendering checks for rsvga, run in the guest.
 *
 * Each test draws into an offscreen target, reads the pixels back and
 * compares them with what Direct3D specifies: a colored triangle, constant
 * buffers and an indexed quad, a texture, depth, blending, instancing, an
 * integer target, scissors, SV_VertexID, multisample resolve, mipmap
 * generation, and last a window with a swap chain (left on screen a few
 * seconds, to be seen). Results go to the host (guest/common/hostlog.h):
 * one "PASS name" or "FAIL name: why" line per test, then a summary.
 *
 * Shaders are HLSL compiled at run time by d3dcompiler_47.dll (part of
 * Windows 10 and 11), for shader model 4.0: rsvga offers feature level
 * 10_1.
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

/* Vertex shader `vs` of `src`, and its input layout if `elements`. */
static ID3D11VertexShader *make_vs(const char *src, const D3D11_INPUT_ELEMENT_DESC *elements,
                                   UINT count, ID3D11InputLayout **layout)
{
    ID3D11VertexShader *vs = NULL;
    ID3DBlob *code = compile(src, "vs", "vs_4_0");

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

static ID3D11PixelShader *make_ps(const char *src)
{
    ID3D11PixelShader *ps = NULL;
    ID3DBlob *code = compile(src, "ps", "ps_4_0");

    if (!code)
        return NULL;
    ok("CreatePixelShader",
       ID3D11Device_CreatePixelShader(dev, ID3D10Blob_GetBufferPointer(code),
                                      ID3D10Blob_GetBufferSize(code), NULL, &ps));
    ID3D10Blob_Release(code);
    return ps;
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
    if (seconds > 0)
        test_window(seconds);
    say("device removed reason: 0x%08lx", (unsigned long)ID3D11Device_GetDeviceRemovedReason(dev));
    say("summary: %d passed, %d failed", passed, failed);
    ID3D11DeviceContext_Release(ctx);
    ID3D11Device_Release(dev);
    log_close();
    return failed ? 1 : 0;
}
