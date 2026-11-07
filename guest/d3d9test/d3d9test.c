/*
 * d3d9test: Direct3D 9 rendering checks for rsvga, run in the guest.
 *
 * VMware's Direct3D 9 driver speaks the legacy SVGA3D protocol (render
 * states, texture stages, transforms, lights, Direct3D 9 shader bytecode),
 * not the DX one d3dtest exercises. Each test here draws into a 64x64
 * offscreen target, reads it back (GetRenderTargetData) and compares the
 * pixels with what Direct3D 9 specifies: clears, the fixed-function
 * pipeline (pretransformed and transformed vertices, texture stages,
 * lighting, alpha test, fog), shader models 2 and 3, indices, depth,
 * blending, scissors, pixel centers, StretchRect, queries; then sRGB,
 * point sprites, clip planes, cube and volume textures, generated mipmaps,
 * multiple render targets, two-sided stencil, vertex fog, instancing,
 * vertex textures, shader model 1 (texbem, texm3x2tex), subroutines and
 * start offsets; last a window presented a few seconds. Results go to the host
 * (guest/common/hostlog.h): one "PASS name" or "FAIL name: why" line per
 * test, then a summary.
 *
 * Shaders are HLSL compiled at run time by d3dcompiler_47.dll (part of
 * Windows 10 and 11).
 *
 * Usage: d3d9test [seconds the window stays, default 5]
 * Build: scripts/build-guest-tools.sh (mingw-w64).
 */
#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d9.h>
#include <d3dcompiler.h>
#include <math.h>
#include <stdlib.h>

#include "../common/hostlog.h"

#define W 64
#define H 64

/* A8R8G8B8 pixels read as little-endian words: 0xAARRGGBB. */
#define ARGB(a, r, g, b) \
    ((uint32_t)(b) | (uint32_t)(g) << 8 | (uint32_t)(r) << 16 | (uint32_t)(a) << 24)
#define SOLID(r, g, b) ARGB(255, r, g, b)
#define BLACK SOLID(0, 0, 0)
#define RED SOLID(255, 0, 0)
#define GREEN SOLID(0, 255, 0)
#define BLUE SOLID(0, 0, 255)
#define WHITE SOLID(255, 255, 255)

#define RELEASE(p)                              \
    do {                                        \
        if (p) {                                \
            IUnknown_Release((IUnknown *)(p));  \
            (p) = NULL;                         \
        }                                       \
    } while (0)

static IDirect3D9 *d3d;
static IDirect3DDevice9 *dev;
static IDirect3DStateBlock9 *defaults;
static IDirect3DSurface9 *rt, *readback;
static HWND device_window;
static pD3DCompile compile_fn;
static int passed, failed;
static const char *test_name;
static int test_failures;
static uint32_t pixels[W * H];
#define AT(x, y) pixels[(y) * W + (x)]

/* ---- test bookkeeping -------------------------------------------------- */

static void begin(const char *name)
{
    D3DVIEWPORT9 vp = {0, 0, W, H, 0, 1};

    test_name = name;
    test_failures = 0;
    IDirect3DStateBlock9_Apply(defaults);
    IDirect3DDevice9_SetRenderTarget(dev, 0, rt);
    IDirect3DDevice9_SetDepthStencilSurface(dev, NULL);
    IDirect3DDevice9_SetViewport(dev, &vp);
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
        fail("compiling %s (%s): hr=0x%08lx %s", entry, target, (unsigned long)hr,
             errors ? (const char *)ID3D10Blob_GetBufferPointer(errors) : "");
    if (errors)
        ID3D10Blob_Release(errors);
    return code;
}

static IDirect3DVertexShader9 *make_vs(const char *src, const char *target)
{
    IDirect3DVertexShader9 *vs = NULL;
    ID3DBlob *code = compile(src, "vs", target);

    if (code) {
        ok("CreateVertexShader",
           IDirect3DDevice9_CreateVertexShader(dev, ID3D10Blob_GetBufferPointer(code), &vs));
        ID3D10Blob_Release(code);
    }
    return vs;
}

static IDirect3DPixelShader9 *make_ps(const char *src, const char *target)
{
    IDirect3DPixelShader9 *ps = NULL;
    ID3DBlob *code = compile(src, "ps", target);

    if (code) {
        ok("CreatePixelShader",
           IDirect3DDevice9_CreatePixelShader(dev, ID3D10Blob_GetBufferPointer(code), &ps));
        ID3D10Blob_Release(code);
    }
    return ps;
}

static IDirect3DVertexBuffer9 *make_vb(const void *data, UINT size)
{
    IDirect3DVertexBuffer9 *vb = NULL;
    void *p;

    if (!ok("CreateVertexBuffer", IDirect3DDevice9_CreateVertexBuffer(
                                      dev, size, D3DUSAGE_WRITEONLY, 0, D3DPOOL_DEFAULT, &vb, NULL)))
        return NULL;
    if (!ok("Lock (vertices)", IDirect3DVertexBuffer9_Lock(vb, 0, 0, &p, 0))) {
        RELEASE(vb);
        return NULL;
    }
    memcpy(p, data, size);
    IDirect3DVertexBuffer9_Unlock(vb);
    return vb;
}

static IDirect3DIndexBuffer9 *make_ib(const WORD *data, UINT size)
{
    IDirect3DIndexBuffer9 *ib = NULL;
    void *p;

    if (!ok("CreateIndexBuffer",
            IDirect3DDevice9_CreateIndexBuffer(dev, size, D3DUSAGE_WRITEONLY, D3DFMT_INDEX16,
                                               D3DPOOL_DEFAULT, &ib, NULL)))
        return NULL;
    if (!ok("Lock (indices)", IDirect3DIndexBuffer9_Lock(ib, 0, 0, &p, 0))) {
        RELEASE(ib);
        return NULL;
    }
    memcpy(p, data, size);
    IDirect3DIndexBuffer9_Unlock(ib);
    return ib;
}

/* A 4x4 managed texture: red, green / blue, white quadrants. */
static IDirect3DTexture9 *make_quadrants(void)
{
    IDirect3DTexture9 *tex = NULL;
    D3DLOCKED_RECT lr;
    int x, y;

    if (!ok("CreateTexture", IDirect3DDevice9_CreateTexture(dev, 4, 4, 1, 0, D3DFMT_A8R8G8B8,
                                                            D3DPOOL_MANAGED, &tex, NULL)))
        return NULL;
    if (!ok("LockRect (texture)", IDirect3DTexture9_LockRect(tex, 0, &lr, NULL, 0))) {
        RELEASE(tex);
        return NULL;
    }
    for (y = 0; y < 4; y++)
        for (x = 0; x < 4; x++)
            ((uint32_t *)((char *)lr.pBits + y * lr.Pitch))[x] =
                y < 2 ? (x < 2 ? RED : GREEN) : (x < 2 ? BLUE : WHITE);
    IDirect3DTexture9_UnlockRect(tex, 0);
    return tex;
}

static int read_surface(IDirect3DSurface9 *src, IDirect3DSurface9 *dest, uint32_t *out, int width,
                        int height)
{
    D3DLOCKED_RECT lr;
    int y;

    if (!ok("GetRenderTargetData", IDirect3DDevice9_GetRenderTargetData(dev, src, dest)) ||
        !ok("LockRect (readback)", IDirect3DSurface9_LockRect(dest, &lr, NULL, D3DLOCK_READONLY)))
        return 0;
    for (y = 0; y < height; y++)
        memcpy(&out[y * width], (const char *)lr.pBits + y * lr.Pitch, (size_t)width * 4);
    IDirect3DSurface9_UnlockRect(dest);
    return 1;
}

static int read_target(void)
{
    return read_surface(rt, readback, pixels, W, H);
}

static void clear(uint32_t color)
{
    ok("Clear", IDirect3DDevice9_Clear(dev, 0, NULL, D3DCLEAR_TARGET, color, 1, 0));
}

/* ---- drawing helpers --------------------------------------------------- */

struct tvertex {
    float x, y, z, rhw;
    DWORD color;
};
#define TVERTEX_FVF (D3DFVF_XYZRHW | D3DFVF_DIFFUSE)

static void draw_up(D3DPRIMITIVETYPE type, UINT count, const void *v, UINT stride)
{
    ok("BeginScene", IDirect3DDevice9_BeginScene(dev));
    ok("DrawPrimitiveUP", IDirect3DDevice9_DrawPrimitiveUP(dev, type, count, v, stride));
    ok("EndScene", IDirect3DDevice9_EndScene(dev));
}

/* A pretransformed rectangle: `left` color on its left edge, `right` on
 * its right one. */
static void rect_rhw(float x0, float y0, float x1, float y1, float z, DWORD left, DWORD right)
{
    struct tvertex v[4] = {
        {x0, y0, z, 1, left}, {x1, y0, z, 1, right}, {x0, y1, z, 1, left}, {x1, y1, z, 1, right},
    };

    IDirect3DDevice9_SetFVF(dev, TVERTEX_FVF);
    draw_up(D3DPT_TRIANGLESTRIP, 2, v, sizeof v[0]);
}

static void identity(D3DMATRIX *m)
{
    memset(m, 0, sizeof *m);
    m->_11 = m->_22 = m->_33 = m->_44 = 1;
}

/* ---- tests ----------------------------------------------------------------- */

static void test_clear(void)
{
    D3DRECT r = {16, 16, 32, 48};

    begin("clear");
    clear(SOLID(32, 64, 128));
    ok("Clear (rectangle)", IDirect3DDevice9_Clear(dev, 1, &r, D3DCLEAR_TARGET, RED, 1, 0));
    if (read_target()) {
        expect("outside", AT(4, 4), SOLID(32, 64, 128), 0);
        expect("inside", AT(20, 30), RED, 0);
        expect("right of it", AT(32, 30), SOLID(32, 64, 128), 0);
    }
    end();
}

/* Pretransformed vertices through the fixed-function pipeline, with
 * colors interpolated across the rectangle. */
static void test_pretransformed(void)
{
    begin("pretransformed");
    clear(BLACK);
    rect_rhw(0, 0, 64, 32, 0.5f, RED, BLUE);
    if (read_target()) {
        expect("left", AT(0, 10), RED, 8);
        expect("middle", AT(32, 10), SOLID(128, 0, 128), 8);
        expect("right", AT(63, 10), BLUE, 8);
        expect("below", AT(32, 40), BLACK, 0);
    }
    end();
}

/* Direct3D 9 puts pixel centers on integer coordinates: a rectangle from
 * -0.5 to 31.5 covers exactly pixels 0 to 31. */
static void test_pixel_centers(void)
{
    begin("pixel centers");
    clear(BLACK);
    rect_rhw(-0.5f, -0.5f, 31.5f, 31.5f, 0.5f, WHITE, WHITE);
    if (read_target()) {
        expect("first", AT(0, 0), WHITE, 0);
        expect("last column", AT(31, 10), WHITE, 0);
        expect("last row", AT(10, 31), WHITE, 0);
        expect("outside", AT(32, 10), BLACK, 0);
        expect("outside below", AT(10, 32), BLACK, 0);
    }
    end();
}

/* World, view and projection transforms of the fixed-function pipeline. */
static void test_transforms(void)
{
    static const struct {
        float x, y, z;
        DWORD color;
    } v[3] = {{0, 0, 0, GREEN}, {1, 0, 0, GREEN}, {0, 1, 0, GREEN}};
    D3DMATRIX world, view, proj;

    begin("transforms");
    clear(BLACK);
    identity(&world);
    identity(&view);
    identity(&proj);
    /* The triangle (0,0) (1,0) (0,1) scaled by 0.5 and moved to the left
     * half: x from -0.5 to 0, y from 0 to 0.5. */
    world._11 = world._22 = 0.5f;
    view._41 = -0.5f;
    IDirect3DDevice9_SetTransform(dev, D3DTS_WORLD, &world);
    IDirect3DDevice9_SetTransform(dev, D3DTS_VIEW, &view);
    IDirect3DDevice9_SetTransform(dev, D3DTS_PROJECTION, &proj);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_LIGHTING, FALSE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_CULLMODE, D3DCULL_NONE);
    IDirect3DDevice9_SetFVF(dev, D3DFVF_XYZ | D3DFVF_DIFFUSE);
    draw_up(D3DPT_TRIANGLELIST, 1, v, sizeof v[0]);
    if (read_target()) {
        /* Clip x -0.5..0 is pixels 16..32, clip y 0..0.5 is rows 16..32. */
        expect("inside", AT(18, 30), GREEN, 0);
        expect("right of it", AT(40, 30), BLACK, 0);
        expect("below it", AT(18, 40), BLACK, 0);
    }
    end();
}

/* A directional light on a material. */
static void test_lighting(void)
{
    static const struct {
        float x, y, z, nx, ny, nz;
    } v[4] = {
        {-1, 1, 0.5f, 0, 0, -1},
        {1, 1, 0.5f, 0, 0, -1},
        {-1, -1, 0.5f, 0, 0, -1},
        {1, -1, 0.5f, 0, 0, -1},
    };
    D3DLIGHT9 light;
    D3DMATERIAL9 mat;

    begin("lighting");
    clear(BLACK);
    memset(&light, 0, sizeof light);
    light.Type = D3DLIGHT_DIRECTIONAL;
    light.Diffuse.r = light.Diffuse.g = light.Diffuse.b = light.Diffuse.a = 1;
    light.Direction.z = 1; /* into the screen, against the normals */
    memset(&mat, 0, sizeof mat);
    mat.Diffuse.r = 1;
    mat.Diffuse.g = 0.5f;
    mat.Diffuse.a = 1;
    IDirect3DDevice9_SetLight(dev, 0, &light);
    IDirect3DDevice9_LightEnable(dev, 0, TRUE);
    IDirect3DDevice9_SetMaterial(dev, &mat);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_LIGHTING, TRUE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_AMBIENT, 0);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_CULLMODE, D3DCULL_NONE);
    IDirect3DDevice9_SetFVF(dev, D3DFVF_XYZ | D3DFVF_NORMAL);
    draw_up(D3DPT_TRIANGLESTRIP, 2, v, sizeof v[0]);
    if (read_target())
        expect("lit", AT(32, 32), SOLID(255, 128, 0), 2);
    end();
}

/* A texture through stage 0 (modulated by the default white diffuse), then
 * added to the vertex color with D3DTOP_ADD. */
static void test_texture_stages(void)
{
    static const struct {
        float x, y, z, rhw;
        DWORD color;
        float u, v;
    } v[4] = {
        {0, 0, 0.5f, 1, SOLID(0, 0, 128), 0, 0},
        {64, 0, 0.5f, 1, SOLID(0, 0, 128), 1, 0},
        {0, 64, 0.5f, 1, SOLID(0, 0, 128), 0, 1},
        {64, 64, 0.5f, 1, SOLID(0, 0, 128), 1, 1},
    };
    static const struct {
        float x, y, z, rhw, u, v;
    } plain[4] = {
        {0, 0, 0.5f, 1, 0, 0},
        {64, 0, 0.5f, 1, 1, 0},
        {0, 64, 0.5f, 1, 0, 1},
        {64, 64, 0.5f, 1, 1, 1},
    };
    IDirect3DTexture9 *tex;

    begin("texture stages");
    tex = make_quadrants();
    if (!tex)
        goto out;
    clear(BLACK);
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)tex);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_MINFILTER, D3DTEXF_POINT);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_MAGFILTER, D3DTEXF_POINT);
    IDirect3DDevice9_SetFVF(dev, D3DFVF_XYZRHW | D3DFVF_TEX1);
    draw_up(D3DPT_TRIANGLESTRIP, 2, plain, sizeof plain[0]);
    if (read_target()) {
        expect("red quadrant", AT(10, 10), RED, 0);
        expect("green quadrant", AT(50, 10), GREEN, 0);
        expect("blue quadrant", AT(10, 50), BLUE, 0);
        expect("white quadrant", AT(50, 50), WHITE, 0);
    }
    clear(BLACK);
    IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_COLOROP, D3DTOP_ADD);
    IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_COLORARG1, D3DTA_TEXTURE);
    IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_COLORARG2, D3DTA_DIFFUSE);
    IDirect3DDevice9_SetFVF(dev, D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_TEX1);
    draw_up(D3DPT_TRIANGLESTRIP, 2, v, sizeof v[0]);
    if (read_target()) {
        expect("red + blue", AT(10, 10), SOLID(255, 0, 128), 1);
        expect("green + blue", AT(50, 10), SOLID(0, 255, 128), 1);
        expect("white + blue", AT(50, 50), WHITE, 0);
    }
out:
    IDirect3DDevice9_SetTexture(dev, 0, NULL);
    RELEASE(tex);
    end();
}

/* Shader model 2: a vertex shader with a matrix and a tint from constants,
 * a pixel shader with a constant, a vertex buffer and a declaration. */
static const char sm2_vs[] =
    "float4x4 transform : register(c0);\n"
    "float4 tint : register(c4);\n"
    "struct out_t { float4 pos : POSITION; float4 color : COLOR0; };\n"
    "out_t vs(float3 pos : POSITION, float4 color : COLOR0)\n"
    "{\n"
    "    out_t o;\n"
    "    o.pos = mul(float4(pos, 1), transform);\n"
    "    o.color = color * tint;\n"
    "    return o;\n"
    "}\n";
static const char sm2_ps[] =
    "float4 scale : register(c0);\n"
    "float4 ps(float4 color : COLOR0) : COLOR0 { return color * scale; }\n";

static void test_shader_model_2(void)
{
    static const struct {
        float x, y, z;
        DWORD color;
    } v[3] = {{-1, 1, 0.5f, WHITE}, {1, 1, 0.5f, WHITE}, {-1, -1, 0.5f, WHITE}};
    static const D3DVERTEXELEMENT9 elements[] = {
        {0, 0, D3DDECLTYPE_FLOAT3, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_POSITION, 0},
        {0, 12, D3DDECLTYPE_D3DCOLOR, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_COLOR, 0},
        D3DDECL_END(),
    };
    /* Transposed (HLSL mul(v, m) reads columns from registers): x and y
     * kept, z kept. */
    static const float transform[16] = {1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1};
    static const float tint[4] = {1, 0.5f, 0, 1};
    static const float scale[4] = {1, 1, 1, 1};
    IDirect3DVertexDeclaration9 *decl = NULL;
    IDirect3DVertexBuffer9 *vb = NULL;
    IDirect3DVertexShader9 *vs;
    IDirect3DPixelShader9 *ps;

    begin("shader model 2");
    vs = make_vs(sm2_vs, "vs_2_0");
    ps = make_ps(sm2_ps, "ps_2_0");
    vb = make_vb(v, sizeof v);
    if (!vs || !ps || !vb ||
        !ok("CreateVertexDeclaration",
            IDirect3DDevice9_CreateVertexDeclaration(dev, elements, &decl)))
        goto out;
    clear(BLACK);
    IDirect3DDevice9_SetVertexDeclaration(dev, decl);
    IDirect3DDevice9_SetStreamSource(dev, 0, vb, 0, sizeof v[0]);
    IDirect3DDevice9_SetVertexShader(dev, vs);
    IDirect3DDevice9_SetPixelShader(dev, ps);
    IDirect3DDevice9_SetVertexShaderConstantF(dev, 0, transform, 4);
    IDirect3DDevice9_SetVertexShaderConstantF(dev, 4, tint, 1);
    IDirect3DDevice9_SetPixelShaderConstantF(dev, 0, scale, 1);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_CULLMODE, D3DCULL_NONE);
    ok("BeginScene", IDirect3DDevice9_BeginScene(dev));
    ok("DrawPrimitive", IDirect3DDevice9_DrawPrimitive(dev, D3DPT_TRIANGLELIST, 0, 1));
    ok("EndScene", IDirect3DDevice9_EndScene(dev));
    if (read_target()) {
        expect("top left", AT(4, 4), SOLID(255, 128, 0), 1);
        expect("bottom right", AT(60, 60), BLACK, 0);
    }
out:
    IDirect3DDevice9_SetVertexShader(dev, NULL);
    IDirect3DDevice9_SetPixelShader(dev, NULL);
    IDirect3DDevice9_SetVertexDeclaration(dev, NULL);
    IDirect3DDevice9_SetStreamSource(dev, 0, NULL, 0, 0);
    RELEASE(decl);
    RELEASE(vb);
    RELEASE(ps);
    RELEASE(vs);
    end();
}

/* Shader model 3: a texture sampled in the pixel shader, an indexed quad
 * with a base vertex, a loop with an integer constant. */
static const char sm3_vs[] =
    "struct out_t { float4 pos : POSITION; float2 uv : TEXCOORD0; };\n"
    "out_t vs(float4 pos : POSITION, float2 uv : TEXCOORD0)\n"
    "{\n"
    "    out_t o;\n"
    "    o.pos = pos;\n"
    "    o.uv = uv;\n"
    "    return o;\n"
    "}\n";
static const char sm3_ps[] =
    "sampler2D s : register(s0);\n"
    "int count : register(i0);\n"
    "float4 ps(float2 uv : TEXCOORD0) : COLOR0\n"
    "{\n"
    "    float4 t = tex2D(s, uv), c = 0;\n"
    "    [loop] for (int i = 0; i < count; i++)\n"
    "        c += t * 0.25;\n"
    "    return c;\n"
    "}\n";

static void test_shader_model_3(void)
{
    /* Two junk vertices first: the draw uses a base vertex of 2. */
    static const struct {
        float x, y, z, w, u, v;
    } v[6] = {
        {0, 0, 0, 1, 0, 0},     {0, 0, 0, 1, 0, 0},     {-1, 1, 0.5f, 1, 0, 0},
        {1, 1, 0.5f, 1, 1, 0}, {-1, -1, 0.5f, 1, 0, 1}, {1, -1, 0.5f, 1, 1, 1},
    };
    static const WORD indices[6] = {0, 1, 2, 2, 1, 3};
    static const D3DVERTEXELEMENT9 elements[] = {
        {0, 0, D3DDECLTYPE_FLOAT4, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_POSITION, 0},
        {0, 16, D3DDECLTYPE_FLOAT2, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_TEXCOORD, 0},
        D3DDECL_END(),
    };
    static const int count[4] = {4, 0, 1, 0};
    IDirect3DVertexDeclaration9 *decl = NULL;
    IDirect3DVertexBuffer9 *vb = NULL;
    IDirect3DIndexBuffer9 *ib = NULL;
    IDirect3DTexture9 *tex = NULL;
    IDirect3DVertexShader9 *vs;
    IDirect3DPixelShader9 *ps;

    begin("shader model 3");
    vs = make_vs(sm3_vs, "vs_3_0");
    ps = make_ps(sm3_ps, "ps_3_0");
    vb = make_vb(v, sizeof v);
    ib = make_ib(indices, sizeof indices);
    tex = make_quadrants();
    if (!vs || !ps || !vb || !ib || !tex ||
        !ok("CreateVertexDeclaration",
            IDirect3DDevice9_CreateVertexDeclaration(dev, elements, &decl)))
        goto out;
    clear(BLACK);
    IDirect3DDevice9_SetVertexDeclaration(dev, decl);
    IDirect3DDevice9_SetStreamSource(dev, 0, vb, 0, sizeof v[0]);
    IDirect3DDevice9_SetIndices(dev, ib);
    IDirect3DDevice9_SetVertexShader(dev, vs);
    IDirect3DDevice9_SetPixelShader(dev, ps);
    IDirect3DDevice9_SetPixelShaderConstantI(dev, 0, count, 1);
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)tex);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_MINFILTER, D3DTEXF_POINT);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_MAGFILTER, D3DTEXF_POINT);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_CULLMODE, D3DCULL_NONE);
    ok("BeginScene", IDirect3DDevice9_BeginScene(dev));
    ok("DrawIndexedPrimitive",
       IDirect3DDevice9_DrawIndexedPrimitive(dev, D3DPT_TRIANGLELIST, 2, 0, 4, 0, 2));
    ok("EndScene", IDirect3DDevice9_EndScene(dev));
    if (read_target()) {
        expect("red quadrant", AT(10, 10), RED, 1);
        expect("green quadrant", AT(50, 10), GREEN, 1);
        expect("blue quadrant", AT(10, 50), BLUE, 1);
        expect("white quadrant", AT(50, 50), WHITE, 1);
    }
out:
    IDirect3DDevice9_SetTexture(dev, 0, NULL);
    IDirect3DDevice9_SetVertexShader(dev, NULL);
    IDirect3DDevice9_SetPixelShader(dev, NULL);
    IDirect3DDevice9_SetVertexDeclaration(dev, NULL);
    IDirect3DDevice9_SetStreamSource(dev, 0, NULL, 0, 0);
    IDirect3DDevice9_SetIndices(dev, NULL);
    RELEASE(decl);
    RELEASE(tex);
    RELEASE(ib);
    RELEASE(vb);
    RELEASE(ps);
    RELEASE(vs);
    end();
}

/* A depth buffer: a near green rectangle hides a farther red one. */
static void test_depth(void)
{
    IDirect3DSurface9 *ds = NULL;

    begin("depth");
    if (!ok("CreateDepthStencilSurface",
            IDirect3DDevice9_CreateDepthStencilSurface(dev, W, H, D3DFMT_D24S8,
                                                       D3DMULTISAMPLE_NONE, 0, TRUE, &ds, NULL)))
        goto out;
    IDirect3DDevice9_SetDepthStencilSurface(dev, ds);
    ok("Clear", IDirect3DDevice9_Clear(dev, 0, NULL, D3DCLEAR_TARGET | D3DCLEAR_ZBUFFER, BLACK,
                                       1, 0));
    IDirect3DDevice9_SetRenderState(dev, D3DRS_ZENABLE, D3DZB_TRUE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_ZFUNC, D3DCMP_LESS);
    rect_rhw(0, 0, 64, 64, 0.5f, GREEN, GREEN);
    rect_rhw(0, 0, 64, 64, 0.7f, RED, RED);
    rect_rhw(0, 0, 32, 64, 0.3f, BLUE, BLUE);
    if (read_target()) {
        expect("nearest left", AT(10, 32), BLUE, 0);
        expect("green kept right", AT(50, 32), GREEN, 0);
    }
out:
    IDirect3DDevice9_SetDepthStencilSurface(dev, NULL);
    RELEASE(ds);
    end();
}

/* Alpha blending and the alpha test. */
static void test_blend(void)
{
    begin("blend");
    clear(BLUE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_ALPHABLENDENABLE, TRUE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_SRCBLEND, D3DBLEND_SRCALPHA);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_DESTBLEND, D3DBLEND_INVSRCALPHA);
    rect_rhw(0, 0, 64, 64, 0.5f, ARGB(128, 255, 0, 0), ARGB(128, 255, 0, 0));
    if (read_target())
        expect("half red over blue", AT(32, 32), ARGB(191, 128, 0, 127), 2);
    end();

    begin("alpha test");
    clear(BLACK);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_ALPHATESTENABLE, TRUE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_ALPHAREF, 128);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_ALPHAFUNC, D3DCMP_GREATEREQUAL);
    /* Alpha from 0 on the left to 255 on the right. */
    rect_rhw(0, 0, 64, 64, 0.5f, ARGB(0, 255, 255, 255), ARGB(255, 255, 255, 255));
    if (read_target()) {
        expect("left, rejected", AT(10, 32), BLACK, 0);
        expect("right, kept", AT(54, 32), ARGB(216, 255, 255, 255), 4);
    }
    end();
}

/* Linear table fog on pretransformed vertices: z = 0.5 is half fogged. */
static void test_fog(void)
{
    union {
        float f;
        DWORD d;
    } start = {0}, finish = {1};

    begin("fog");
    clear(BLACK);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_FOGENABLE, TRUE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_FOGCOLOR, BLUE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_FOGTABLEMODE, D3DFOG_LINEAR);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_FOGSTART, start.d);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_FOGEND, finish.d);
    rect_rhw(0, 0, 64, 64, 0.5f, RED, RED);
    if (read_target())
        expect("half fogged", AT(32, 32), SOLID(128, 0, 127), 3);
    end();
}

/* The scissor rectangle and a viewport smaller than the target. */
static void test_scissor_viewport(void)
{
    RECT scissor = {0, 0, 32, 64};
    D3DVIEWPORT9 vp = {32, 32, 32, 32, 0, 1};
    D3DRECT all = {0, 0, W, H};

    begin("scissor");
    clear(BLACK);
    IDirect3DDevice9_SetScissorRect(dev, &scissor);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_SCISSORTESTENABLE, TRUE);
    rect_rhw(0, 0, 64, 64, 0.5f, GREEN, GREEN);
    if (read_target()) {
        expect("inside", AT(10, 32), GREEN, 0);
        expect("outside", AT(50, 32), BLACK, 0);
    }
    end();

    begin("viewport");
    clear(BLACK);
    IDirect3DDevice9_SetViewport(dev, &vp);
    /* Clear only touches the viewport, even with a bigger rectangle. */
    ok("Clear", IDirect3DDevice9_Clear(dev, 1, &all, D3DCLEAR_TARGET, RED, 1, 0));
    if (read_target()) {
        expect("viewport cleared", AT(40, 40), RED, 0);
        expect("outside kept", AT(10, 10), BLACK, 0);
    }
    end();
}

/* StretchRect: a scaled copy between render targets, and a color fill. */
static void test_stretch_rect(void)
{
    IDirect3DSurface9 *small = NULL;
    RECT src = {0, 0, 16, 16}, dest = {0, 0, 32, 32};
    RECT fill = {32, 0, 64, 32};

    begin("stretch rect");
    if (!ok("CreateRenderTarget",
            IDirect3DDevice9_CreateRenderTarget(dev, 16, 16, D3DFMT_A8R8G8B8, D3DMULTISAMPLE_NONE,
                                                0, FALSE, &small, NULL)))
        goto out;
    IDirect3DDevice9_SetRenderTarget(dev, 0, small);
    clear(GREEN);
    IDirect3DDevice9_SetRenderTarget(dev, 0, rt);
    clear(BLACK);
    ok("StretchRect", IDirect3DDevice9_StretchRect(dev, small, &src, rt, &dest, D3DTEXF_POINT));
    ok("ColorFill", IDirect3DDevice9_ColorFill(dev, rt, &fill, BLUE));
    if (read_target()) {
        expect("stretched", AT(10, 10), GREEN, 0);
        expect("filled", AT(40, 10), BLUE, 0);
        expect("untouched", AT(10, 40), BLACK, 0);
    }
out:
    RELEASE(small);
    end();
}

/* An event query completes; an occlusion query counts pixels. */
static void test_queries(void)
{
    IDirect3DQuery9 *event = NULL, *occlusion = NULL;
    DWORD start, samples = 0;
    BOOL done = FALSE;
    HRESULT hr;

    begin("queries");
    if (!ok("CreateQuery (event)",
            IDirect3DDevice9_CreateQuery(dev, D3DQUERYTYPE_EVENT, &event)))
        goto out;
    clear(BLACK);
    IDirect3DQuery9_Issue(event, D3DISSUE_END);
    start = GetTickCount();
    while ((hr = IDirect3DQuery9_GetData(event, &done, sizeof done, D3DGETDATA_FLUSH)) == S_FALSE &&
           GetTickCount() - start < 3000)
        Sleep(1);
    if (hr != S_OK || !done)
        fail("event query: hr=0x%08lx after %lu ms", (unsigned long)hr,
             (unsigned long)(GetTickCount() - start));
    if (!ok("CreateQuery (occlusion)",
            IDirect3DDevice9_CreateQuery(dev, D3DQUERYTYPE_OCCLUSION, &occlusion)))
        goto out;
    IDirect3DQuery9_Issue(occlusion, D3DISSUE_BEGIN);
    rect_rhw(0, 0, 16, 16, 0.5f, WHITE, WHITE);
    IDirect3DQuery9_Issue(occlusion, D3DISSUE_END);
    start = GetTickCount();
    while ((hr = IDirect3DQuery9_GetData(occlusion, &samples, sizeof samples,
                                         D3DGETDATA_FLUSH)) == S_FALSE &&
           GetTickCount() - start < 3000)
        Sleep(1);
    if (hr != S_OK)
        fail("occlusion query: hr=0x%08lx", (unsigned long)hr);
    else if (samples != 256)
        fail("occlusion query: %lu samples, expected 256", (unsigned long)samples);
out:
    RELEASE(occlusion);
    RELEASE(event);
    end();
}

/* ---- beyond the basics ----------------------------------------------------- */

static DWORD fbits(float f)
{
    union {
        float f;
        DWORD d;
    } u = {f};
    return u.d;
}

static IDirect3DPixelShader9 *ps_tokens(const DWORD *tokens)
{
    IDirect3DPixelShader9 *ps = NULL;

    ok("CreatePixelShader (tokens)", IDirect3DDevice9_CreatePixelShader(dev, tokens, &ps));
    return ps;
}

/* A managed texture of one level filled with `texel` (32-bit formats). */
static IDirect3DTexture9 *make_solid(D3DFORMAT format, UINT size, uint32_t texel)
{
    IDirect3DTexture9 *tex = NULL;
    D3DLOCKED_RECT lr;
    UINT x, y;

    if (!ok("CreateTexture", IDirect3DDevice9_CreateTexture(dev, size, size, 1, 0, format,
                                                            D3DPOOL_MANAGED, &tex, NULL)))
        return NULL;
    if (!ok("LockRect", IDirect3DTexture9_LockRect(tex, 0, &lr, NULL, 0))) {
        RELEASE(tex);
        return NULL;
    }
    for (y = 0; y < size; y++)
        for (x = 0; x < size; x++)
            ((uint32_t *)((char *)lr.pBits + y * lr.Pitch))[x] = texel;
    IDirect3DTexture9_UnlockRect(tex, 0);
    return tex;
}

struct uv_vertex {
    float x, y, z, rhw, u, v;
};

/* A full-target quad of pretransformed vertices with texture coordinates. */
static void textured_rect(void)
{
    static const struct uv_vertex q[4] = {
        {0, 0, 0.5f, 1, 0, 0},
        {64, 0, 0.5f, 1, 1, 0},
        {0, 64, 0.5f, 1, 0, 1},
        {64, 64, 0.5f, 1, 1, 1},
    };

    IDirect3DDevice9_SetFVF(dev, D3DFVF_XYZRHW | D3DFVF_TEX1);
    draw_up(D3DPT_TRIANGLESTRIP, 2, q, sizeof q[0]);
}

/* sRGB texture reads and render target writes: 128 in sRGB is 55 linear. */
static void test_srgb(void)
{
    IDirect3DTexture9 *tex;

    begin("srgb read");
    tex = make_solid(D3DFMT_A8R8G8B8, 1, SOLID(128, 128, 128));
    if (tex) {
        clear(BLACK);
        IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)tex);
        IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_SRGBTEXTURE, TRUE);
        textured_rect();
        if (read_target())
            expect("decoded", AT(32, 32), SOLID(55, 55, 55), 3);
        IDirect3DDevice9_SetTexture(dev, 0, NULL);
    }
    RELEASE(tex);
    end();

    begin("srgb write");
    clear(BLACK);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_SRGBWRITEENABLE, TRUE);
    rect_rhw(0, 0, 64, 64, 0.5f, SOLID(55, 55, 55), SOLID(55, 55, 55));
    if (read_target())
        expect("encoded", AT(32, 32), SOLID(128, 128, 128), 3);
    end();
}

/* A 16-pixel point sprite: the texture spread over the square. */
static void test_point_sprites(void)
{
    struct {
        float x, y, z, rhw;
        DWORD color;
        float u, v;
    } p = {32, 32, 0.5f, 1, WHITE, 0, 0};
    IDirect3DTexture9 *tex;

    begin("point sprites");
    tex = make_quadrants();
    if (!tex)
        goto out;
    clear(BLACK);
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)tex);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_POINTSIZE, fbits(16));
    IDirect3DDevice9_SetRenderState(dev, D3DRS_POINTSPRITEENABLE, TRUE);
    IDirect3DDevice9_SetFVF(dev, D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_TEX1);
    draw_up(D3DPT_POINTLIST, 1, &p, sizeof p);
    if (read_target()) {
        expect("top left", AT(26, 26), RED, 0);
        expect("top right", AT(37, 26), GREEN, 0);
        expect("bottom left", AT(26, 37), BLUE, 0);
        expect("bottom right", AT(37, 37), WHITE, 0);
        expect("outside", AT(20, 32), BLACK, 0);
        expect("outside", AT(42, 32), BLACK, 0);
    }
    IDirect3DDevice9_SetTexture(dev, 0, NULL);
out:
    RELEASE(tex);
    end();
}

static const char pass_vs[] =
    "struct out_t { float4 pos : POSITION; float4 color : COLOR0; };\n"
    "out_t vs(float4 pos : POSITION, float4 color : COLOR0)\n"
    "{\n"
    "    out_t o;\n"
    "    o.pos = pos;\n"
    "    o.color = color;\n"
    "    return o;\n"
    "}\n";
static const char pass_ps[] = "float4 ps(float4 c : COLOR0) : COLOR0 { return c; }\n";

struct color_vertex {
    float x, y, z;
    DWORD color;
};

/* A user clip plane (clip space with a vertex shader): x >= 0 is kept. */
static void test_clip_planes(void)
{
    static const struct color_vertex q[4] = {
        {-1, 1, 0.5f, GREEN}, {1, 1, 0.5f, GREEN}, {-1, -1, 0.5f, GREEN}, {1, -1, 0.5f, GREEN},
    };
    static const float plane[4] = {1, 0, 0, 0};
    IDirect3DVertexShader9 *vs;
    IDirect3DPixelShader9 *ps;

    begin("clip plane");
    vs = make_vs(pass_vs, "vs_2_0");
    ps = make_ps(pass_ps, "ps_2_0");
    if (!vs || !ps)
        goto out;
    clear(BLACK);
    IDirect3DDevice9_SetVertexShader(dev, vs);
    IDirect3DDevice9_SetPixelShader(dev, ps);
    IDirect3DDevice9_SetFVF(dev, D3DFVF_XYZ | D3DFVF_DIFFUSE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_CULLMODE, D3DCULL_NONE);
    IDirect3DDevice9_SetClipPlane(dev, 0, plane);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_CLIPPLANEENABLE, D3DCLIPPLANE0);
    draw_up(D3DPT_TRIANGLESTRIP, 2, q, sizeof q[0]);
    if (read_target()) {
        expect("clipped", AT(10, 32), BLACK, 0);
        expect("kept", AT(54, 32), GREEN, 0);
    }
out:
    IDirect3DDevice9_SetRenderState(dev, D3DRS_CLIPPLANEENABLE, 0);
    IDirect3DDevice9_SetVertexShader(dev, NULL);
    IDirect3DDevice9_SetPixelShader(dev, NULL);
    RELEASE(ps);
    RELEASE(vs);
    end();
}

/* Cube and volume textures, sampled by ps_2_0 shaders. */
static void test_cube_volume(void)
{
    static const char cube_ps[] =
        "samplerCUBE s : register(s0);\n"
        "float4 ps(float3 d : TEXCOORD0) : COLOR0 { return texCUBE(s, d); }\n";
    static const char volume_ps[] =
        "sampler3D s : register(s0);\n"
        "float4 ps(float3 d : TEXCOORD0) : COLOR0 { return tex3D(s, d); }\n";
    static const uint32_t faces[6] = {RED, GREEN, BLUE, WHITE, SOLID(255, 255, 0),
                                      SOLID(0, 255, 255)};
    struct {
        float x, y, z, rhw, u, v, w;
    } q[8] = {
        {0, 0, 0.5f, 1, 1, 0, 0},  {32, 0, 0.5f, 1, 1, 0, 0},  {0, 64, 0.5f, 1, 1, 0, 0},
        {32, 64, 0.5f, 1, 1, 0, 0}, {32, 0, 0.5f, 1, 0, 0, -1}, {64, 0, 0.5f, 1, 0, 0, -1},
        {32, 64, 0.5f, 1, 0, 0, -1}, {64, 64, 0.5f, 1, 0, 0, -1},
    };
    IDirect3DCubeTexture9 *cube = NULL;
    IDirect3DVolumeTexture9 *volume = NULL;
    IDirect3DPixelShader9 *ps = NULL;
    D3DLOCKED_RECT lr;
    D3DLOCKED_BOX lb;
    DWORD fvf = D3DFVF_XYZRHW | D3DFVF_TEX1 | D3DFVF_TEXCOORDSIZE3(0);
    int i;

    begin("cube texture");
    ps = make_ps(cube_ps, "ps_2_0");
    if (!ps || !ok("CreateCubeTexture",
                   IDirect3DDevice9_CreateCubeTexture(dev, 1, 1, 0, D3DFMT_A8R8G8B8,
                                                      D3DPOOL_MANAGED, &cube, NULL)))
        goto cube_out;
    for (i = 0; i < 6; i++) {
        if (!ok("LockRect (face)",
                IDirect3DCubeTexture9_LockRect(cube, (D3DCUBEMAP_FACES)i, 0, &lr, NULL, 0)))
            goto cube_out;
        *(uint32_t *)lr.pBits = faces[i];
        IDirect3DCubeTexture9_UnlockRect(cube, (D3DCUBEMAP_FACES)i, 0);
    }
    clear(BLACK);
    IDirect3DDevice9_SetPixelShader(dev, ps);
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)cube);
    IDirect3DDevice9_SetFVF(dev, fvf);
    draw_up(D3DPT_TRIANGLESTRIP, 2, q, sizeof q[0]);
    draw_up(D3DPT_TRIANGLESTRIP, 2, q + 4, sizeof q[0]);
    if (read_target()) {
        expect("+x face", AT(16, 32), RED, 0);
        expect("-z face", AT(48, 32), SOLID(0, 255, 255), 0);
    }
cube_out:
    IDirect3DDevice9_SetTexture(dev, 0, NULL);
    IDirect3DDevice9_SetPixelShader(dev, NULL);
    RELEASE(cube);
    RELEASE(ps);
    end();

    begin("volume texture");
    ps = make_ps(volume_ps, "ps_2_0");
    if (!ps || !ok("CreateVolumeTexture",
                   IDirect3DDevice9_CreateVolumeTexture(dev, 2, 2, 2, 1, 0, D3DFMT_A8R8G8B8,
                                                        D3DPOOL_MANAGED, &volume, NULL)) ||
        !ok("LockBox", IDirect3DVolumeTexture9_LockBox(volume, 0, &lb, NULL, 0)))
        goto volume_out;
    for (i = 0; i < 8; i++)
        ((uint32_t *)((char *)lb.pBits + (i / 4) * lb.SlicePitch + ((i / 2) % 2) * lb.RowPitch))[i % 2] =
            i < 4 ? RED : BLUE;
    IDirect3DVolumeTexture9_UnlockBox(volume, 0);
    for (i = 0; i < 8; i++) {
        q[i].u = 0.5f;
        q[i].v = 0.5f;
        q[i].w = i < 4 ? 0.25f : 0.75f;
    }
    clear(BLACK);
    IDirect3DDevice9_SetPixelShader(dev, ps);
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)volume);
    IDirect3DDevice9_SetFVF(dev, fvf);
    draw_up(D3DPT_TRIANGLESTRIP, 2, q, sizeof q[0]);
    draw_up(D3DPT_TRIANGLESTRIP, 2, q + 4, sizeof q[0]);
    if (read_target()) {
        expect("first slice", AT(16, 32), RED, 0);
        expect("second slice", AT(48, 32), BLUE, 0);
    }
volume_out:
    IDirect3DDevice9_SetTexture(dev, 0, NULL);
    IDirect3DDevice9_SetPixelShader(dev, NULL);
    RELEASE(volume);
    RELEASE(ps);
    end();
}

/* GenerateMipSubLevels on a checkerboard, sampled at its 1x1 level. */
static void test_autogen_mips(void)
{
    IDirect3DTexture9 *tex = NULL;
    D3DLOCKED_RECT lr;
    int x, y;

    begin("autogen mipmaps");
    if (!ok("CreateTexture (autogen)",
            IDirect3DDevice9_CreateTexture(dev, 4, 4, 0, D3DUSAGE_AUTOGENMIPMAP, D3DFMT_A8R8G8B8,
                                           D3DPOOL_MANAGED, &tex, NULL)) ||
        !ok("LockRect", IDirect3DTexture9_LockRect(tex, 0, &lr, NULL, 0)))
        goto out;
    for (y = 0; y < 4; y++)
        for (x = 0; x < 4; x++)
            ((uint32_t *)((char *)lr.pBits + y * lr.Pitch))[x] = (x + y) % 2 ? WHITE : BLACK;
    IDirect3DTexture9_UnlockRect(tex, 0);
    IDirect3DTexture9_GenerateMipSubLevels(tex);
    clear(BLUE);
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)tex);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_MIPFILTER, D3DTEXF_POINT);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_MAXMIPLEVEL, 2);
    textured_rect();
    if (read_target())
        expect("1x1 level", AT(32, 32), SOLID(128, 128, 128), 3);
out:
    IDirect3DDevice9_SetTexture(dev, 0, NULL);
    RELEASE(tex);
    end();
}

/* Two render targets written by one pixel shader. */
static void test_mrt(DWORD targets)
{
    static const char mrt_ps[] =
        "struct out_t { float4 a : COLOR0; float4 b : COLOR1; };\n"
        "out_t ps(float4 c : COLOR0) { out_t o; o.a = c; o.b = c.bgra; return o; }\n";
    IDirect3DSurface9 *rt2 = NULL;
    IDirect3DPixelShader9 *ps = NULL;
    static uint32_t second[W * H];

    begin("multiple render targets");
    if (targets < 2) {
        say("note %s: %lu simultaneous render targets", test_name, (unsigned long)targets);
        goto out;
    }
    ps = make_ps(mrt_ps, "ps_2_0");
    if (!ps ||
        !ok("CreateRenderTarget", IDirect3DDevice9_CreateRenderTarget(
                                      dev, W, H, D3DFMT_A8R8G8B8, D3DMULTISAMPLE_NONE, 0, FALSE,
                                      &rt2, NULL)))
        goto out;
    IDirect3DDevice9_SetRenderTarget(dev, 1, rt2);
    ok("Clear", IDirect3DDevice9_Clear(dev, 0, NULL, D3DCLEAR_TARGET, BLACK, 1, 0));
    IDirect3DDevice9_SetPixelShader(dev, ps);
    rect_rhw(0, 0, 64, 64, 0.5f, RED, RED);
    if (read_target())
        expect("first target", AT(32, 32), RED, 0);
    if (read_surface(rt2, readback, second, W, H))
        expect("second target", second[32 * W + 32], BLUE, 0);
out:
    IDirect3DDevice9_SetPixelShader(dev, NULL);
    IDirect3DDevice9_SetRenderTarget(dev, 1, NULL);
    RELEASE(ps);
    RELEASE(rt2);
    end();
}

/* Two-sided stencil: clockwise triangles count 1, counterclockwise ones
 * write the reference 5; then each value selects a color. */
static void test_two_sided_stencil(void)
{
    struct tvertex ccw[4] = {
        {32, 0, 0.5f, 1, WHITE}, {32, 64, 0.5f, 1, WHITE},
        {64, 0, 0.5f, 1, WHITE}, {64, 64, 0.5f, 1, WHITE},
    };
    IDirect3DSurface9 *ds = NULL;

    begin("two-sided stencil");
    if (!ok("CreateDepthStencilSurface",
            IDirect3DDevice9_CreateDepthStencilSurface(dev, W, H, D3DFMT_D24S8,
                                                       D3DMULTISAMPLE_NONE, 0, TRUE, &ds, NULL)))
        goto out;
    IDirect3DDevice9_SetDepthStencilSurface(dev, ds);
    ok("Clear", IDirect3DDevice9_Clear(dev, 0, NULL, D3DCLEAR_TARGET | D3DCLEAR_STENCIL, BLACK, 1,
                                       0));
    IDirect3DDevice9_SetRenderState(dev, D3DRS_ZENABLE, D3DZB_FALSE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_CULLMODE, D3DCULL_NONE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILENABLE, TRUE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_TWOSIDEDSTENCILMODE, TRUE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILFUNC, D3DCMP_ALWAYS);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILPASS, D3DSTENCILOP_INCR);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_CCW_STENCILFUNC, D3DCMP_ALWAYS);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_CCW_STENCILPASS, D3DSTENCILOP_REPLACE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILREF, 5);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_COLORWRITEENABLE, 0);
    rect_rhw(0, 0, 32, 64, 0.5f, WHITE, WHITE);
    IDirect3DDevice9_SetFVF(dev, TVERTEX_FVF);
    draw_up(D3DPT_TRIANGLESTRIP, 2, ccw, sizeof ccw[0]);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_COLORWRITEENABLE, 0xf);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_TWOSIDEDSTENCILMODE, FALSE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILFUNC, D3DCMP_EQUAL);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILPASS, D3DSTENCILOP_KEEP);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILREF, 1);
    rect_rhw(0, 0, 64, 64, 0.5f, GREEN, GREEN);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILREF, 5);
    rect_rhw(0, 0, 64, 64, 0.5f, BLUE, BLUE);
    if (read_target()) {
        expect("clockwise", AT(10, 32), GREEN, 0);
        expect("counterclockwise", AT(54, 32), BLUE, 0);
    }
out:
    IDirect3DDevice9_SetDepthStencilSurface(dev, NULL);
    RELEASE(ds);
    end();
}

/* Linear vertex fog of the fixed-function pipeline, from view-space depth. */
static void test_vertex_fog(void)
{
    static const struct color_vertex q[4] = {
        {-1, 1, 0.5f, RED}, {1, 1, 0.5f, RED}, {-1, -1, 0.5f, RED}, {1, -1, 0.5f, RED},
    };

    begin("vertex fog");
    clear(BLACK);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_LIGHTING, FALSE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_CULLMODE, D3DCULL_NONE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_FOGENABLE, TRUE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_FOGCOLOR, BLUE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_FOGVERTEXMODE, D3DFOG_LINEAR);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_FOGSTART, fbits(0));
    IDirect3DDevice9_SetRenderState(dev, D3DRS_FOGEND, fbits(1));
    IDirect3DDevice9_SetFVF(dev, D3DFVF_XYZ | D3DFVF_DIFFUSE);
    draw_up(D3DPT_TRIANGLESTRIP, 2, q, sizeof q[0]);
    if (read_target())
        expect("half fogged", AT(32, 32), SOLID(128, 0, 127), 3);
    end();
}

/* Hardware instancing: one quad, two instances with their own offset and
 * color. */
static void test_instancing(void)
{
    static const char vs_src[] =
        "struct out_t { float4 pos : POSITION; float4 color : COLOR0; };\n"
        "out_t vs(float2 pos : POSITION, float2 offset : TEXCOORD0, float4 color : COLOR0)\n"
        "{\n"
        "    out_t o;\n"
        "    o.pos = float4(pos + offset, 0.5, 1);\n"
        "    o.color = color;\n"
        "    return o;\n"
        "}\n";
    static const float quad[8] = {-0.25f, 0.25f, 0.25f, 0.25f, -0.25f, -0.25f, 0.25f, -0.25f};
    static const struct {
        float x, y;
        DWORD color;
    } instances[2] = {{-0.5f, 0.5f, RED}, {0.5f, -0.5f, BLUE}};
    static const WORD indices[6] = {0, 1, 2, 2, 1, 3};
    static const D3DVERTEXELEMENT9 elements[] = {
        {0, 0, D3DDECLTYPE_FLOAT2, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_POSITION, 0},
        {1, 0, D3DDECLTYPE_FLOAT2, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_TEXCOORD, 0},
        {1, 8, D3DDECLTYPE_D3DCOLOR, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_COLOR, 0},
        D3DDECL_END(),
    };
    IDirect3DVertexDeclaration9 *decl = NULL;
    IDirect3DVertexBuffer9 *geometry = NULL, *per_instance = NULL;
    IDirect3DIndexBuffer9 *ib = NULL;
    IDirect3DVertexShader9 *vs;
    IDirect3DPixelShader9 *ps;

    begin("instancing");
    vs = make_vs(vs_src, "vs_3_0");
    ps = make_ps(pass_ps, "ps_3_0");
    geometry = make_vb(quad, sizeof quad);
    per_instance = make_vb(instances, sizeof instances);
    ib = make_ib(indices, sizeof indices);
    if (!vs || !ps || !geometry || !per_instance || !ib ||
        !ok("CreateVertexDeclaration",
            IDirect3DDevice9_CreateVertexDeclaration(dev, elements, &decl)))
        goto out;
    clear(BLACK);
    IDirect3DDevice9_SetVertexDeclaration(dev, decl);
    IDirect3DDevice9_SetStreamSource(dev, 0, geometry, 0, 8);
    IDirect3DDevice9_SetStreamSource(dev, 1, per_instance, 0, sizeof instances[0]);
    IDirect3DDevice9_SetStreamSourceFreq(dev, 0, D3DSTREAMSOURCE_INDEXEDDATA | 2);
    IDirect3DDevice9_SetStreamSourceFreq(dev, 1, D3DSTREAMSOURCE_INSTANCEDATA | 1);
    IDirect3DDevice9_SetIndices(dev, ib);
    IDirect3DDevice9_SetVertexShader(dev, vs);
    IDirect3DDevice9_SetPixelShader(dev, ps);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_CULLMODE, D3DCULL_NONE);
    ok("BeginScene", IDirect3DDevice9_BeginScene(dev));
    ok("DrawIndexedPrimitive",
       IDirect3DDevice9_DrawIndexedPrimitive(dev, D3DPT_TRIANGLELIST, 0, 0, 4, 0, 2));
    ok("EndScene", IDirect3DDevice9_EndScene(dev));
    if (read_target()) {
        expect("first instance", AT(16, 16), RED, 0);
        expect("second instance", AT(48, 48), BLUE, 0);
        expect("between", AT(32, 32), BLACK, 0);
    }
out:
    IDirect3DDevice9_SetStreamSourceFreq(dev, 0, 1);
    IDirect3DDevice9_SetStreamSourceFreq(dev, 1, 1);
    IDirect3DDevice9_SetStreamSource(dev, 0, NULL, 0, 0);
    IDirect3DDevice9_SetStreamSource(dev, 1, NULL, 0, 0);
    IDirect3DDevice9_SetIndices(dev, NULL);
    IDirect3DDevice9_SetVertexDeclaration(dev, NULL);
    IDirect3DDevice9_SetVertexShader(dev, NULL);
    IDirect3DDevice9_SetPixelShader(dev, NULL);
    RELEASE(decl);
    RELEASE(ib);
    RELEASE(per_instance);
    RELEASE(geometry);
    RELEASE(ps);
    RELEASE(vs);
    end();
}

/* A texture read by the vertex shader (tex2Dlod). */
static void test_vertex_texture(void)
{
    static const char vs_src[] =
        "sampler2D t : register(s0);\n"
        "struct out_t { float4 pos : POSITION; float4 color : COLOR0; };\n"
        "out_t vs(float4 pos : POSITION)\n"
        "{\n"
        "    out_t o;\n"
        "    o.pos = pos;\n"
        "    o.color = tex2Dlod(t, float4(0.5, 0.5, 0, 0));\n"
        "    return o;\n"
        "}\n";
    static const struct color_vertex q[4] = {
        {-1, 1, 0.5f, 0}, {1, 1, 0.5f, 0}, {-1, -1, 0.5f, 0}, {1, -1, 0.5f, 0},
    };
    IDirect3DTexture9 *tex = NULL;
    IDirect3DVertexShader9 *vs;
    IDirect3DPixelShader9 *ps;
    HRESULT hr;

    begin("vertex texture");
    hr = IDirect3D9_CheckDeviceFormat(d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, D3DFMT_X8R8G8B8,
                                      D3DUSAGE_QUERY_VERTEXTEXTURE, D3DRTYPE_TEXTURE,
                                      D3DFMT_A8R8G8B8);
    if (FAILED(hr))
        say("note %s: A8R8G8B8 vertex textures: hr=0x%08lx", test_name, (unsigned long)hr);
    vs = make_vs(vs_src, "vs_3_0");
    ps = make_ps(pass_ps, "ps_3_0");
    tex = make_solid(D3DFMT_A8R8G8B8, 1, GREEN);
    if (!vs || !ps || !tex)
        goto out;
    clear(BLACK);
    IDirect3DDevice9_SetTexture(dev, D3DVERTEXTEXTURESAMPLER0, (IDirect3DBaseTexture9 *)tex);
    IDirect3DDevice9_SetVertexShader(dev, vs);
    IDirect3DDevice9_SetPixelShader(dev, ps);
    IDirect3DDevice9_SetFVF(dev, D3DFVF_XYZ | D3DFVF_DIFFUSE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_CULLMODE, D3DCULL_NONE);
    draw_up(D3DPT_TRIANGLESTRIP, 2, q, sizeof q[0]);
    if (read_target())
        expect("fetched", AT(32, 32), GREEN, 0);
out:
    IDirect3DDevice9_SetTexture(dev, D3DVERTEXTEXTURESAMPLER0, NULL);
    IDirect3DDevice9_SetVertexShader(dev, NULL);
    IDirect3DDevice9_SetPixelShader(dev, NULL);
    RELEASE(tex);
    RELEASE(ps);
    RELEASE(vs);
    end();
}

/* Shader model 1 pixel shaders, assembled by hand (d3dcompiler_47 no
 * longer makes them): tex and arithmetic, ps_1_4 texld, texbem, and the
 * texm3x2pad / texm3x2tex pair. */
static void test_shader_model_1(void)
{
    /* ps_1_1: tex t0; mul r0, t0, v0 */
    static const DWORD ps11[] = {0xffff0101, 0x00000042, 0xb00f0000, 0x00000005, 0x800f0000,
                                 0xb0e40000, 0x90e40000, 0x0000ffff};
    /* ps_1_4: texld r0, t0 */
    static const DWORD ps14[] = {0xffff0104, 0x00000042, 0x800f0000, 0xb0e40000, 0x0000ffff};
    /* ps_1_1: tex t0; texbem t1, t0; mov r0, t1 */
    static const DWORD bem[] = {0xffff0101, 0x00000042, 0xb00f0000, 0x00000043, 0xb00f0001,
                                0xb0e40000, 0x00000001, 0x800f0000, 0xb0e40001, 0x0000ffff};
    /* ps_1_1: tex t0; texm3x2pad t1, t0_bx2; texm3x2tex t2, t0_bx2; mov r0, t2 */
    static const DWORD m3x2[] = {0xffff0101, 0x00000042, 0xb00f0000, 0x00000047, 0xb00f0001,
                                 0xb4e40000, 0x00000048, 0xb00f0002, 0xb4e40000, 0x00000001,
                                 0x800f0000, 0xb0e40002, 0x0000ffff};
    struct {
        float x, y, z, rhw;
        DWORD color;
        float u, v;
    } q[4] = {
        {0, 0, 0.5f, 1, SOLID(128, 128, 128), 0, 0},
        {64, 0, 0.5f, 1, SOLID(128, 128, 128), 1, 0},
        {0, 64, 0.5f, 1, SOLID(128, 128, 128), 0, 1},
        {64, 64, 0.5f, 1, SOLID(128, 128, 128), 1, 1},
    };
    struct {
        float x, y, z, rhw, u0, v0, u1, v1;
    } b[4] = {
        {0, 0, 0.5f, 1, 0, 0, 0, 0},
        {64, 0, 0.5f, 1, 1, 0, 1, 0},
        {0, 64, 0.5f, 1, 0, 1, 0, 1},
        {64, 64, 0.5f, 1, 1, 1, 1, 1},
    };
    struct {
        float x, y, z, rhw, u0, v0, s1, t1, r1, s2, t2, r2;
    } m[4];
    IDirect3DTexture9 *tex = NULL, *bump = NULL, *normal = NULL;
    IDirect3DPixelShader9 *ps = NULL;
    D3DLOCKED_RECT lr;
    int i;

    begin("ps_1_1");
    tex = make_quadrants();
    ps = ps_tokens(ps11);
    if (!tex || !ps)
        goto out;
    clear(BLACK);
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)tex);
    IDirect3DDevice9_SetPixelShader(dev, ps);
    IDirect3DDevice9_SetFVF(dev, D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_TEX1);
    draw_up(D3DPT_TRIANGLESTRIP, 2, q, sizeof q[0]);
    if (read_target()) {
        expect("red quadrant", AT(10, 10), SOLID(128, 0, 0), 2);
        expect("white quadrant", AT(50, 50), SOLID(128, 128, 128), 2);
    }
    end();

    begin("ps_1_4");
    RELEASE(ps);
    ps = ps_tokens(ps14);
    if (!ps)
        goto out;
    clear(BLACK);
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)tex);
    IDirect3DDevice9_SetPixelShader(dev, ps);
    IDirect3DDevice9_SetFVF(dev, D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_TEX1);
    draw_up(D3DPT_TRIANGLESTRIP, 2, q, sizeof q[0]);
    if (read_target()) {
        expect("green quadrant", AT(50, 10), GREEN, 0);
        expect("blue quadrant", AT(10, 50), BLUE, 0);
    }
    end();

    begin("texbem");
    RELEASE(ps);
    ps = ps_tokens(bem);
    if (!ps || !ok("CreateTexture (V8U8)",
                   IDirect3DDevice9_CreateTexture(dev, 1, 1, 1, 0, D3DFMT_V8U8, D3DPOOL_MANAGED,
                                                  &bump, NULL)) ||
        !ok("LockRect (V8U8)", IDirect3DTexture9_LockRect(bump, 0, &lr, NULL, 0)))
        goto out;
    /* du = +0.5, dv = 0. */
    ((signed char *)lr.pBits)[0] = 64;
    ((signed char *)lr.pBits)[1] = 0;
    IDirect3DTexture9_UnlockRect(bump, 0);
    clear(BLACK);
    IDirect3DDevice9_SetPixelShader(dev, ps);
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)bump);
    IDirect3DDevice9_SetTexture(dev, 1, (IDirect3DBaseTexture9 *)tex);
    IDirect3DDevice9_SetSamplerState(dev, 1, D3DSAMP_ADDRESSU, D3DTADDRESS_CLAMP);
    IDirect3DDevice9_SetTextureStageState(dev, 1, D3DTSS_BUMPENVMAT00, fbits(1));
    IDirect3DDevice9_SetTextureStageState(dev, 1, D3DTSS_BUMPENVMAT01, fbits(0));
    IDirect3DDevice9_SetTextureStageState(dev, 1, D3DTSS_BUMPENVMAT10, fbits(0));
    IDirect3DDevice9_SetTextureStageState(dev, 1, D3DTSS_BUMPENVMAT11, fbits(1));
    IDirect3DDevice9_SetFVF(dev, D3DFVF_XYZRHW | D3DFVF_TEX2);
    draw_up(D3DPT_TRIANGLESTRIP, 2, b, sizeof b[0]);
    if (read_target()) {
        expect("shifted to green", AT(10, 10), GREEN, 0);
        expect("shifted to white", AT(10, 50), WHITE, 0);
    }
    IDirect3DDevice9_SetTexture(dev, 1, NULL);
    end();

    begin("texm3x2tex");
    RELEASE(ps);
    ps = ps_tokens(m3x2);
    /* The normal (1, 0, 0) as unsigned texels. */
    normal = make_solid(D3DFMT_A8R8G8B8, 1, SOLID(255, 128, 128));
    if (!ps || !normal)
        goto out;
    for (i = 0; i < 4; i++) {
        m[i].x = i % 2 ? 64.0f : 0.0f;
        m[i].y = i / 2 ? 64.0f : 0.0f;
        m[i].z = 0.5f;
        m[i].rhw = 1;
        m[i].u0 = m[i].v0 = 0;
        /* u = 0.8 and v = 0.2 everywhere: the green quadrant. */
        m[i].s1 = 0.8f;
        m[i].t1 = m[i].r1 = 0;
        m[i].s2 = 0.2f;
        m[i].t2 = m[i].r2 = 0;
    }
    clear(BLACK);
    IDirect3DDevice9_SetPixelShader(dev, ps);
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)normal);
    IDirect3DDevice9_SetTexture(dev, 2, (IDirect3DBaseTexture9 *)tex);
    IDirect3DDevice9_SetFVF(dev, D3DFVF_XYZRHW | D3DFVF_TEX3 | D3DFVF_TEXCOORDSIZE3(1) |
                                     D3DFVF_TEXCOORDSIZE3(2));
    draw_up(D3DPT_TRIANGLESTRIP, 2, m, sizeof m[0]);
    if (read_target())
        expect("looked up", AT(32, 32), GREEN, 2);
    IDirect3DDevice9_SetTexture(dev, 2, NULL);
out:
    IDirect3DDevice9_SetTexture(dev, 0, NULL);
    IDirect3DDevice9_SetPixelShader(dev, NULL);
    RELEASE(normal);
    RELEASE(bump);
    RELEASE(ps);
    RELEASE(tex);
    end();
}

/* A ps_3_0 subroutine (call, label, ret), assembled by hand. */
static void test_subroutine(void)
{
    static const char vs_src[] = "float4 vs(float4 p : POSITION) : POSITION { return p; }\n";
    static const DWORD ps30[] = {
        0xffff0300,
        0x05000051, 0xa00f0000, 0x00000000, 0x3f800000, 0x00000000, 0x3f800000, /* def c0 */
        0x01000019, 0xa0e41000, /* call l0 */
        0x02000001, 0x800f0800, 0x80e40000, /* mov oC0, r0 */
        0x0000001c, /* ret */
        0x0100001e, 0xa0e41000, /* label l0 */
        0x02000001, 0x800f0000, 0xa0e40000, /* mov r0, c0 */
        0x0000001c, /* ret */
        0x0000ffff,
    };
    static const struct color_vertex q[4] = {
        {-1, 1, 0.5f, 0}, {1, 1, 0.5f, 0}, {-1, -1, 0.5f, 0}, {1, -1, 0.5f, 0},
    };
    IDirect3DVertexShader9 *vs;
    IDirect3DPixelShader9 *ps;

    begin("subroutine");
    vs = make_vs(vs_src, "vs_3_0");
    ps = ps_tokens(ps30);
    if (!vs || !ps)
        goto out;
    clear(BLACK);
    IDirect3DDevice9_SetVertexShader(dev, vs);
    IDirect3DDevice9_SetPixelShader(dev, ps);
    IDirect3DDevice9_SetFVF(dev, D3DFVF_XYZ | D3DFVF_DIFFUSE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_CULLMODE, D3DCULL_NONE);
    draw_up(D3DPT_TRIANGLESTRIP, 2, q, sizeof q[0]);
    if (read_target())
        expect("called", AT(32, 32), GREEN, 0);
out:
    IDirect3DDevice9_SetVertexShader(dev, NULL);
    IDirect3DDevice9_SetPixelShader(dev, NULL);
    RELEASE(ps);
    RELEASE(vs);
    end();
}

/* DrawPrimitive from a start vertex, DrawIndexedPrimitive from a start
 * index: the skipped data would cover the right half in red. */
static void test_start_offsets(void)
{
    static const struct tvertex v[6] = {
        {32, 0, 0.5f, 1, RED},  {64, 0, 0.5f, 1, RED},  {32, 64, 0.5f, 1, RED},
        {0, 0, 0.5f, 1, GREEN}, {32, 0, 0.5f, 1, GREEN}, {0, 64, 0.5f, 1, GREEN},
    };
    static const WORD indices[6] = {0, 1, 2, 3, 4, 5};
    IDirect3DVertexBuffer9 *vb;
    IDirect3DIndexBuffer9 *ib;

    begin("start vertex and index");
    vb = make_vb(v, sizeof v);
    ib = make_ib(indices, sizeof indices);
    if (!vb || !ib)
        goto out;
    IDirect3DDevice9_SetFVF(dev, TVERTEX_FVF);
    IDirect3DDevice9_SetStreamSource(dev, 0, vb, 0, sizeof v[0]);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_CULLMODE, D3DCULL_NONE);
    clear(BLACK);
    ok("BeginScene", IDirect3DDevice9_BeginScene(dev));
    ok("DrawPrimitive", IDirect3DDevice9_DrawPrimitive(dev, D3DPT_TRIANGLELIST, 3, 1));
    ok("EndScene", IDirect3DDevice9_EndScene(dev));
    if (read_target()) {
        expect("start vertex 3", AT(8, 8), GREEN, 0);
        expect("skipped vertices", AT(50, 8), BLACK, 0);
    }
    clear(BLACK);
    IDirect3DDevice9_SetIndices(dev, ib);
    ok("BeginScene", IDirect3DDevice9_BeginScene(dev));
    ok("DrawIndexedPrimitive",
       IDirect3DDevice9_DrawIndexedPrimitive(dev, D3DPT_TRIANGLELIST, 0, 0, 6, 3, 1));
    ok("EndScene", IDirect3DDevice9_EndScene(dev));
    if (read_target()) {
        expect("start index 3", AT(8, 8), GREEN, 0);
        expect("skipped indices", AT(50, 8), BLACK, 0);
    }
out:
    IDirect3DDevice9_SetStreamSource(dev, 0, NULL, 0, 0);
    IDirect3DDevice9_SetIndices(dev, NULL);
    RELEASE(ib);
    RELEASE(vb);
    end();
}

/* Depth textures: a D24S8 shadow map (texldp compares z with the stored
 * depth) and INTZ (reads the depth). The left half holds depth 0.25. */
static void test_depth_textures(void)
{
    static const char shadow_ps[] =
        "sampler2D s : register(s0);\n"
        "float4 ps(float2 uv : TEXCOORD0) : COLOR0 { return tex2Dproj(s, float4(uv, 0.5, 1)); }\n";
    static const char read_ps[] =
        "sampler2D s : register(s0);\n"
        "float4 ps(float2 uv : TEXCOORD0) : COLOR0 { return tex2D(s, uv).r; }\n";
    static const D3DFORMAT formats[2] = {D3DFMT_D24S8, (D3DFORMAT)MAKEFOURCC('I', 'N', 'T', 'Z')};
    static const char *names[2] = {"shadow map", "INTZ depth texture"};
    int i;

    for (i = 0; i < 2; i++) {
        IDirect3DTexture9 *tex = NULL;
        IDirect3DSurface9 *ds = NULL;
        IDirect3DPixelShader9 *ps = NULL;
        HRESULT hr;

        begin(names[i]);
        hr = IDirect3D9_CheckDeviceFormat(d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL,
                                          D3DFMT_X8R8G8B8, D3DUSAGE_DEPTHSTENCIL, D3DRTYPE_TEXTURE,
                                          formats[i]);
        if (FAILED(hr)) {
            fail("CheckDeviceFormat: hr=0x%08lx", (unsigned long)hr);
            goto next;
        }
        ps = make_ps(i == 0 ? shadow_ps : read_ps, "ps_2_0");
        if (!ps || !ok("CreateTexture (depth)",
                       IDirect3DDevice9_CreateTexture(dev, W, H, 1, D3DUSAGE_DEPTHSTENCIL,
                                                      formats[i], D3DPOOL_DEFAULT, &tex, NULL)) ||
            !ok("GetSurfaceLevel", IDirect3DTexture9_GetSurfaceLevel(tex, 0, &ds)))
            goto next;
        IDirect3DDevice9_SetDepthStencilSurface(dev, ds);
        ok("Clear", IDirect3DDevice9_Clear(dev, 0, NULL, D3DCLEAR_TARGET | D3DCLEAR_ZBUFFER, BLACK,
                                           1, 0));
        IDirect3DDevice9_SetRenderState(dev, D3DRS_ZENABLE, D3DZB_TRUE);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_ZFUNC, D3DCMP_ALWAYS);
        rect_rhw(0, 0, 32, 64, 0.25f, WHITE, WHITE);
        IDirect3DDevice9_SetDepthStencilSurface(dev, NULL);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_ZENABLE, D3DZB_FALSE);
        clear(BLUE);
        IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)tex);
        IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_MINFILTER, D3DTEXF_POINT);
        IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_MAGFILTER, D3DTEXF_POINT);
        IDirect3DDevice9_SetPixelShader(dev, ps);
        textured_rect();
        if (read_target()) {
            if (i == 0) {
                /* The comparison in every component, alpha too. */
                expect("in shadow", AT(10, 32), ARGB(0, 0, 0, 0), 0);
                expect("lit", AT(54, 32), WHITE, 0);
            } else {
                expect("depth 0.25", AT(10, 32), ARGB(64, 64, 64, 64), 2);
                expect("depth 1", AT(54, 32), WHITE, 1);
            }
        }
    next:
        IDirect3DDevice9_SetTexture(dev, 0, NULL);
        IDirect3DDevice9_SetPixelShader(dev, NULL);
        IDirect3DDevice9_SetDepthStencilSurface(dev, NULL);
        RELEASE(ps);
        RELEASE(ds);
        RELEASE(tex);
        end();
    }
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

static HWND make_window(const char *title, int visible, int width, int height)
{
    WNDCLASSA wc;
    RECT r = {0, 0, width, height};

    memset(&wc, 0, sizeof wc);
    wc.lpfnWndProc = window_proc;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.lpszClassName = "d3d9test";
    wc.hCursor = LoadCursor(NULL, IDC_ARROW);
    RegisterClassA(&wc);
    AdjustWindowRect(&r, WS_OVERLAPPEDWINDOW, FALSE);
    return CreateWindowA("d3d9test", title, WS_OVERLAPPEDWINDOW | (visible ? WS_VISIBLE : 0),
                         240, 180, r.right - r.left, r.bottom - r.top, NULL, NULL, wc.hInstance,
                         NULL);
}

/* A swap chain of its own in a visible window, presented every frame. */
static void test_window(int seconds)
{
    D3DPRESENT_PARAMETERS pp;
    IDirect3DSwapChain9 *chain = NULL;
    IDirect3DSurface9 *back = NULL, *copy = NULL;
    static uint32_t window_pixels[320 * 240];
    HWND hwnd;
    DWORD start;
    int frames = 0, checked = 0;
    float t = 0;

    begin("window");
    hwnd = make_window("rsvga d3d9test", 1, 320, 240);
    if (!hwnd) {
        fail("CreateWindow: error %lu", GetLastError());
        goto out;
    }
    memset(&pp, 0, sizeof pp);
    pp.BackBufferWidth = 320;
    pp.BackBufferHeight = 240;
    pp.BackBufferFormat = D3DFMT_X8R8G8B8;
    pp.BackBufferCount = 1;
    pp.SwapEffect = D3DSWAPEFFECT_DISCARD;
    pp.hDeviceWindow = hwnd;
    pp.Windowed = TRUE;
    pp.PresentationInterval = D3DPRESENT_INTERVAL_ONE;
    if (!ok("CreateAdditionalSwapChain",
            IDirect3DDevice9_CreateAdditionalSwapChain(dev, &pp, &chain)) ||
        !ok("GetBackBuffer",
            IDirect3DSwapChain9_GetBackBuffer(chain, 0, D3DBACKBUFFER_TYPE_MONO, &back)) ||
        !ok("CreateOffscreenPlainSurface",
            IDirect3DDevice9_CreateOffscreenPlainSurface(dev, 320, 240, D3DFMT_X8R8G8B8,
                                                         D3DPOOL_SYSTEMMEM, &copy, NULL)))
        goto out;
    start = GetTickCount();
    while (IsWindow(hwnd) && GetTickCount() - start < (DWORD)seconds * 1000) {
        D3DVIEWPORT9 vp = {0, 0, 320, 240, 0, 1};
        float c = cosf(t), s = sinf(t);
        struct tvertex v[3] = {
            {160 - 100 * s, 120 - 100 * c, 0.5f, 1, RED},
            {160 + 100 * (0.866f * c + 0.5f * s), 120 + 100 * (0.5f * c - 0.866f * s), 0.5f, 1,
             GREEN},
            {160 - 100 * (0.866f * c - 0.5f * s), 120 + 100 * (0.5f * c + 0.866f * s), 0.5f, 1,
             BLUE},
        };
        MSG msg;
        HRESULT hr;

        while (PeekMessageA(&msg, NULL, 0, 0, PM_REMOVE)) {
            TranslateMessage(&msg);
            DispatchMessageA(&msg);
        }
        IDirect3DDevice9_SetRenderTarget(dev, 0, back);
        IDirect3DDevice9_SetViewport(dev, &vp);
        clear(SOLID(26, 26, 38));
        IDirect3DDevice9_SetFVF(dev, TVERTEX_FVF);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_CULLMODE, D3DCULL_NONE);
        draw_up(D3DPT_TRIANGLELIST, 1, v, sizeof v[0]);
        if (!checked) {
            checked = 1;
            if (read_surface(back, copy, window_pixels, 320, 240)) {
                expect("background", window_pixels[5 * 320 + 5] | 0xff000000, SOLID(26, 26, 38),
                       1);
                expect("center", window_pixels[120 * 320 + 160] | 0xff000000,
                       SOLID(85, 85, 85), 12);
            }
        }
        hr = IDirect3DSwapChain9_Present(chain, NULL, NULL, NULL, NULL, 0);
        if (FAILED(hr)) {
            fail("Present: hr=0x%08lx after %d frames", (unsigned long)hr, frames);
            break;
        }
        frames++;
        t += 0.02f;
    }
    say("note window: %d frames in %d s", frames, seconds);
out:
    IDirect3DDevice9_SetRenderTarget(dev, 0, rt);
    RELEASE(copy);
    RELEASE(back);
    RELEASE(chain);
    if (hwnd && IsWindow(hwnd))
        DestroyWindow(hwnd);
    end();
}

/* ---- main ------------------------------------------------------------------ */

int main(int argc, char **argv)
{
    int seconds = argc > 1 ? atoi(argv[1]) : 5;
    D3DPRESENT_PARAMETERS pp;
    D3DADAPTER_IDENTIFIER9 id;
    D3DCAPS9 caps;
    HMODULE compiler;
    HRESULT hr;

    log_open("d3d9test");
    compiler = LoadLibraryA("d3dcompiler_47.dll");
    compile_fn = compiler ? (pD3DCompile)(void *)GetProcAddress(compiler, "D3DCompile") : NULL;
    if (!compile_fn) {
        say("FAIL setup: no d3dcompiler_47.dll (error %lu)", GetLastError());
        log_close();
        return 2;
    }
    d3d = Direct3DCreate9(D3D_SDK_VERSION);
    if (!d3d) {
        say("FAIL setup: Direct3DCreate9 failed");
        log_close();
        return 2;
    }
    if (SUCCEEDED(IDirect3D9_GetAdapterIdentifier(d3d, D3DADAPTER_DEFAULT, 0, &id)))
        say("start: %s (%s)", id.Description, id.Driver);
    device_window = make_window("d3d9test device", 0, W, H);
    memset(&pp, 0, sizeof pp);
    pp.BackBufferWidth = W;
    pp.BackBufferHeight = H;
    pp.BackBufferFormat = D3DFMT_X8R8G8B8;
    pp.BackBufferCount = 1;
    pp.SwapEffect = D3DSWAPEFFECT_DISCARD;
    pp.hDeviceWindow = device_window;
    pp.Windowed = TRUE;
    pp.PresentationInterval = D3DPRESENT_INTERVAL_IMMEDIATE;
    hr = IDirect3D9_CreateDevice(d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, device_window,
                                 D3DCREATE_HARDWARE_VERTEXPROCESSING, &pp, &dev);
    if (FAILED(hr)) {
        say("FAIL setup: CreateDevice hr=0x%08lx", (unsigned long)hr);
        log_close();
        return 2;
    }
    memset(&caps, 0, sizeof caps);
    if (SUCCEEDED(IDirect3DDevice9_GetDeviceCaps(dev, &caps))) {
        say("start: vertex shader %lx, pixel shader %lx", (unsigned long)caps.VertexShaderVersion,
            (unsigned long)caps.PixelShaderVersion);
        say("start: %lu render targets, point size %.0f, %lu clip planes, decl types %lx",
            (unsigned long)caps.NumSimultaneousRTs, caps.MaxPointSize,
            (unsigned long)caps.MaxUserClipPlanes, (unsigned long)caps.DeclTypes);
    }
    if (FAILED(IDirect3DDevice9_CreateStateBlock(dev, D3DSBT_ALL, &defaults)) ||
        FAILED(IDirect3DDevice9_CreateRenderTarget(dev, W, H, D3DFMT_A8R8G8B8, D3DMULTISAMPLE_NONE,
                                                   0, FALSE, &rt, NULL)) ||
        FAILED(IDirect3DDevice9_CreateOffscreenPlainSurface(dev, W, H, D3DFMT_A8R8G8B8,
                                                            D3DPOOL_SYSTEMMEM, &readback, NULL))) {
        say("FAIL setup: state block, render target or readback surface");
        log_close();
        return 2;
    }
    test_clear();
    test_pretransformed();
    test_pixel_centers();
    test_transforms();
    test_lighting();
    test_texture_stages();
    test_shader_model_2();
    test_shader_model_3();
    test_depth();
    test_blend();
    test_fog();
    test_scissor_viewport();
    test_stretch_rect();
    test_queries();
    test_srgb();
    test_point_sprites();
    test_clip_planes();
    test_cube_volume();
    test_autogen_mips();
    test_mrt(caps.NumSimultaneousRTs);
    test_two_sided_stencil();
    test_vertex_fog();
    test_instancing();
    test_vertex_texture();
    test_shader_model_1();
    test_subroutine();
    test_start_offsets();
    test_depth_textures();
    if (seconds > 0)
        test_window(seconds);
    say("device state: 0x%08lx", (unsigned long)IDirect3DDevice9_TestCooperativeLevel(dev));
    say("summary: %d passed, %d failed", passed, failed);
    RELEASE(readback);
    RELEASE(rt);
    RELEASE(defaults);
    RELEASE(dev);
    RELEASE(d3d);
    log_close();
    return failed ? 1 : 0;
}
