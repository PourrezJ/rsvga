/*
 * videoprobe: what VMware's Direct3D 11 driver sends for video processor
 * blits (VideoProcessorBlt), to decode the command it uses: a series of
 * blits, each changing one parameter (rectangles, sizes, formats, array
 * slices and mips, background color, alpha, color spaces, rotation, frame
 * format, a second stream). Run it with rsvga tracing: the commands appear
 * in rsvga.log in the order of the "blit N" lines this program sends to
 * the host (guest/common/hostlog.h). It also reports the processor's caps
 * and which formats it takes, and two pixels of each result.
 *
 * Build: scripts/build-guest-tools.sh (mingw-w64).
 */
#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d11.h>
#include <stdlib.h>

#include "../common/hostlog.h"

/* Probes list only the fields they set. */
#pragma GCC diagnostic ignored "-Wmissing-field-initializers"

static ID3D11Device *dev;
static ID3D11DeviceContext *ctx;
static ID3D11VideoDevice *vdev;
static ID3D11VideoContext *vctx;

#define RELEASE(p)                                \
    do {                                          \
        if (p)                                    \
            IUnknown_Release((IUnknown *)(p));    \
        (p) = NULL;                               \
    } while (0)

struct probe {
    const char *name;
    UINT sw, sh, smips, sarray, smip, sslice; /* source texture and view */
    DXGI_FORMAT sfmt;
    UINT dw, dh, darray, dslice;              /* target texture and view */
    DXGI_FORMAT dfmt;
    RECT *src, *dst, *target;                 /* rectangles, if set */
    D3D11_VIDEO_COLOR *background;
    BOOL background_ycbcr;
    float alpha;                              /* stream alpha, if > 0 */
    int rotation;                             /* D3D11_VIDEO_PROCESSOR_ROTATION, if > 0 */
    D3D11_VIDEO_PROCESSOR_COLOR_SPACE *in_cs, *out_cs;
    int frame_format;                         /* if > 0 */
    int fill_mode;                            /* output alpha fill mode, if > 0 */
    int two_streams;
};

static UINT format_support(ID3D11VideoProcessorEnumerator *vpe, DXGI_FORMAT f)
{
    UINT flags = 0;
    if (FAILED(ID3D11VideoProcessorEnumerator_CheckVideoProcessorFormat(vpe, f, &flags)))
        return 0;
    return flags;
}

/* A texture for the processor; if `fill`, BGRA ones hold left red, right
 * blue, and NV12 ones a BT.601 red. */
static ID3D11Texture2D *texture(UINT w, UINT h, UINT mips, UINT array, DXGI_FORMAT f, UINT bind,
                                int fill)
{
    D3D11_TEXTURE2D_DESC d = {w, h, mips, array, f, {1, 0}, D3D11_USAGE_DEFAULT, bind, 0, 0};
    ID3D11Texture2D *t = NULL;
    HRESULT hr;

    /* Packed 4:2:2 is read, never rendered to (VMware's driver fails
     * internally on a YUY2 render target). */
    if (f == DXGI_FORMAT_YUY2)
        d.BindFlags = bind = D3D11_BIND_SHADER_RESOURCE;
    hr = ID3D11Device_CreateTexture2D(dev, &d, NULL, &t);

    if (FAILED(hr) && bind != D3D11_BIND_SHADER_RESOURCE) {
        /* YUY2 cannot be a render target. */
        d.BindFlags = D3D11_BIND_SHADER_RESOURCE;
        hr = ID3D11Device_CreateTexture2D(dev, &d, NULL, &t);
    }
    if (FAILED(hr)) {
        say("  CreateTexture2D %ux%u format %d: hr=0x%08lx", w, h, (int)f, (unsigned long)hr);
        return NULL;
    }
    if (fill && f == DXGI_FORMAT_NV12 && mips == 1 && array == 1) {
        unsigned char *px = malloc((size_t)w * h * 3 / 2);
        UINT i;
        memset(px, 81, (size_t)w * h);
        for (i = 0; i < w * h / 2; i += 2) {
            px[w * h + i] = 90;
            px[w * h + i + 1] = 240;
        }
        ID3D11DeviceContext_UpdateSubresource(ctx, (ID3D11Resource *)t, 0, NULL, px, w, 0);
        free(px);
    }
    if (fill && f == DXGI_FORMAT_YUY2 && mips == 1 && array == 1) {
        unsigned char *px = malloc((size_t)w * h * 2);
        UINT i;
        for (i = 0; i < w * h * 2; i += 4) {
            px[i] = px[i + 2] = 81; /* Y0, Y1 */
            px[i + 1] = 90;         /* U */
            px[i + 3] = 240;        /* V: BT.601 red */
        }
        ID3D11DeviceContext_UpdateSubresource(ctx, (ID3D11Resource *)t, 0, NULL, px, w * 2, 0);
        free(px);
    }
    if (fill && f == DXGI_FORMAT_B8G8R8A8_UNORM && mips == 1 && array == 1) {
        uint32_t *px = malloc((size_t)w * h * 4);
        UINT i;
        for (i = 0; i < w * h; i++)
            px[i] = i % w < w / 2 ? 0xffff0000u : 0xff0000ffu;
        ID3D11DeviceContext_UpdateSubresource(ctx, (ID3D11Resource *)t, 0, NULL, px, w * 4, 0);
        free(px);
    }
    return t;
}

static ID3D11VideoProcessorInputView *input_view(ID3D11Texture2D *t,
                                                 ID3D11VideoProcessorEnumerator *vpe, UINT mip,
                                                 UINT slice)
{
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC d = {0, D3D11_VPIV_DIMENSION_TEXTURE2D,
                                               .Texture2D = {mip, slice}};
    ID3D11VideoProcessorInputView *v = NULL;
    HRESULT hr =
        ID3D11VideoDevice_CreateVideoProcessorInputView(vdev, (ID3D11Resource *)t, vpe, &d, &v);
    if (FAILED(hr))
        say("  CreateVideoProcessorInputView: hr=0x%08lx", (unsigned long)hr);
    return v;
}

static void read_two(ID3D11Texture2D *t, UINT w, UINT h, UINT slice, DXGI_FORMAT f)
{
    D3D11_TEXTURE2D_DESC d;
    D3D11_MAPPED_SUBRESOURCE map;
    ID3D11Texture2D *staging = NULL;
    const uint32_t *row;

    ID3D11Texture2D_GetDesc(t, &d);
    d.Usage = D3D11_USAGE_STAGING;
    d.BindFlags = 0;
    d.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    d.MiscFlags = 0;
    if (FAILED(ID3D11Device_CreateTexture2D(dev, &d, NULL, &staging)))
        return;
    ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)staging, (ID3D11Resource *)t);
    if (SUCCEEDED(ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)staging,
                                          slice * d.MipLevels,
                                          D3D11_MAP_READ, 0, &map))) {
        row = (const uint32_t *)((const char *)map.pData + (size_t)(h / 2) * map.RowPitch);
        if (f == DXGI_FORMAT_B8G8R8A8_UNORM || f == DXGI_FORMAT_R8G8B8A8_UNORM)
            say("  result: (%u,%u) %08x, (%u,%u) %08x", w / 4, h / 2, row[w / 4], 3 * w / 4,
                h / 2, row[3 * w / 4]);
        ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)staging, 0);
    }
    RELEASE(staging);
}

static void run(int n, const struct probe *p)
{
    D3D11_VIDEO_PROCESSOR_CONTENT_DESC content = {
        D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, {30, 1}, p->sw, p->sh, {30, 1}, p->dw, p->dh,
        D3D11_VIDEO_USAGE_PLAYBACK_NORMAL};
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC od;
    D3D11_VIDEO_PROCESSOR_STREAM streams[2];
    ID3D11VideoProcessorEnumerator *vpe = NULL;
    ID3D11VideoProcessor *vp = NULL;
    ID3D11Texture2D *src = NULL, *src2 = NULL, *dst = NULL;
    ID3D11VideoProcessorInputView *in = NULL, *in2 = NULL;
    ID3D11VideoProcessorOutputView *out = NULL;
    HRESULT hr;

    say("blit %d: %s", n, p->name);
    if (FAILED(hr = ID3D11VideoDevice_CreateVideoProcessorEnumerator(vdev, &content, &vpe)) ||
        FAILED(hr = ID3D11VideoDevice_CreateVideoProcessor(vdev, vpe, 0, &vp))) {
        say("  processor: hr=0x%08lx", (unsigned long)hr);
        goto out;
    }
    src = texture(p->sw, p->sh, p->smips, p->sarray, p->sfmt,
                  D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE, 1);
    dst = texture(p->dw, p->dh, 1, p->darray, p->dfmt,
                  D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE, 0);
    if (!src || !dst)
        goto out;
    in = input_view(src, vpe, p->smip, p->sslice);
    if (p->darray > 1) {
        od.ViewDimension = D3D11_VPOV_DIMENSION_TEXTURE2DARRAY;
        od.Texture2DArray.MipSlice = 0;
        od.Texture2DArray.FirstArraySlice = p->dslice;
        od.Texture2DArray.ArraySize = 1;
    } else {
        od.ViewDimension = D3D11_VPOV_DIMENSION_TEXTURE2D;
        od.Texture2D.MipSlice = 0;
    }
    hr = ID3D11VideoDevice_CreateVideoProcessorOutputView(vdev, (ID3D11Resource *)dst, vpe, &od,
                                                          &out);
    if (FAILED(hr))
        say("  CreateVideoProcessorOutputView: hr=0x%08lx", (unsigned long)hr);
    if (!in || !out)
        goto out;
    if (p->src)
        ID3D11VideoContext_VideoProcessorSetStreamSourceRect(vctx, vp, 0, TRUE, p->src);
    if (p->dst)
        ID3D11VideoContext_VideoProcessorSetStreamDestRect(vctx, vp, 0, TRUE, p->dst);
    if (p->target)
        ID3D11VideoContext_VideoProcessorSetOutputTargetRect(vctx, vp, TRUE, p->target);
    if (p->background)
        ID3D11VideoContext_VideoProcessorSetOutputBackgroundColor(vctx, vp, p->background_ycbcr,
                                                                  p->background);
    if (p->alpha > 0)
        ID3D11VideoContext_VideoProcessorSetStreamAlpha(vctx, vp, 0, TRUE, p->alpha);
    if (p->rotation > 0)
        ID3D11VideoContext_VideoProcessorSetStreamRotation(vctx, vp, 0, TRUE,
                                                           (D3D11_VIDEO_PROCESSOR_ROTATION)p->rotation);
    if (p->in_cs)
        ID3D11VideoContext_VideoProcessorSetStreamColorSpace(vctx, vp, 0, p->in_cs);
    if (p->out_cs)
        ID3D11VideoContext_VideoProcessorSetOutputColorSpace(vctx, vp, p->out_cs);
    if (p->frame_format > 0)
        ID3D11VideoContext_VideoProcessorSetStreamFrameFormat(vctx, vp, 0,
                                                              (D3D11_VIDEO_FRAME_FORMAT)p->frame_format);
    if (p->fill_mode > 0)
        ID3D11VideoContext_VideoProcessorSetOutputAlphaFillMode(
            vctx, vp, (D3D11_VIDEO_PROCESSOR_ALPHA_FILL_MODE)p->fill_mode, 0);
    memset(streams, 0, sizeof streams);
    streams[0].Enable = TRUE;
    streams[0].pInputSurface = in;
    if (p->two_streams) {
        RECT small = {40, 40, 56, 56};
        src2 = texture(16, 16, 1, 1, DXGI_FORMAT_B8G8R8A8_UNORM,
                       D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE, 1);
        if (src2)
            in2 = input_view(src2, vpe, 0, 0);
        streams[1].Enable = TRUE;
        streams[1].pInputSurface = in2;
        ID3D11VideoContext_VideoProcessorSetStreamDestRect(vctx, vp, 1, TRUE, &small);
    }
    hr = ID3D11VideoContext_VideoProcessorBlt(vctx, vp, out, 0, p->two_streams && in2 ? 2 : 1,
                                              streams);
    say("  VideoProcessorBlt: hr=0x%08lx", (unsigned long)hr);
    if (SUCCEEDED(hr))
        read_two(dst, p->dw, p->dh, p->dslice, p->dfmt);
out:
    RELEASE(out);
    RELEASE(in2);
    RELEASE(in);
    RELEASE(src2);
    RELEASE(src);
    RELEASE(dst);
    RELEASE(vp);
    RELEASE(vpe);
}

int main(void)
{
    static const struct { DXGI_FORMAT f; const char *name; } formats[] = {
        {DXGI_FORMAT_B8G8R8A8_UNORM, "B8G8R8A8"}, {DXGI_FORMAT_B8G8R8X8_UNORM, "B8G8R8X8"},
        {DXGI_FORMAT_R8G8B8A8_UNORM, "R8G8B8A8"}, {DXGI_FORMAT_R10G10B10A2_UNORM, "R10G10B10A2"},
        {DXGI_FORMAT_R16G16B16A16_FLOAT, "R16G16B16A16F"}, {DXGI_FORMAT_NV12, "NV12"},
        {DXGI_FORMAT_YUY2, "YUY2"}, {DXGI_FORMAT_P010, "P010"}, {DXGI_FORMAT_AYUV, "AYUV"},
        {DXGI_FORMAT_420_OPAQUE, "420_OPAQUE"},
    };
    D3D11_VIDEO_PROCESSOR_CONTENT_DESC content = {
        D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, {30, 1}, 32, 32, {30, 1}, 64, 64,
        D3D11_VIDEO_USAGE_PLAYBACK_NORMAL};
    D3D11_VIDEO_PROCESSOR_CAPS caps;
    ID3D11VideoProcessorEnumerator *vpe = NULL;
    D3D_FEATURE_LEVEL level;
    RECT src = {4, 6, 20, 30}, dst = {8, 10, 56, 50}, target = {2, 3, 60, 61};
    D3D11_VIDEO_COLOR bg = {.RGBA = {0.25f, 0.5f, 0.75f, 1.0f}};
    D3D11_VIDEO_COLOR bg_ycbcr = {.YCbCr = {0.5f, 0.25f, 0.75f, 1.0f}};
    D3D11_VIDEO_PROCESSOR_COLOR_SPACE limited = {0, 1, 1, 0, 2, 0};
    D3D11_VIDEO_PROCESSOR_COLOR_SPACE video = {1, 0, 1, 0, 1, 0};
    const DXGI_FORMAT bgra = DXGI_FORMAT_B8G8R8A8_UNORM;
    unsigned i;
    int n = 0;

    log_open("videoprobe");
    if (FAILED(D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL,
                                 D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                                 NULL, 0, D3D11_SDK_VERSION, &dev, &level, &ctx)) ||
        FAILED(ID3D11Device_QueryInterface(dev, &IID_ID3D11VideoDevice, (void **)&vdev)) ||
        FAILED(ID3D11DeviceContext_QueryInterface(ctx, &IID_ID3D11VideoContext, (void **)&vctx))) {
        say("no video device");
        say("--- end");
        log_close();
        return 1;
    }
    say("feature level %x", (unsigned)level);
    if (SUCCEEDED(ID3D11VideoDevice_CreateVideoProcessorEnumerator(vdev, &content, &vpe))) {
        if (SUCCEEDED(ID3D11VideoProcessorEnumerator_GetVideoProcessorCaps(vpe, &caps)))
            say("caps: device %x feature %x filter %x input %x auto %x stereo %x rates %u "
                "streams %u states %u",
                caps.DeviceCaps, caps.FeatureCaps, caps.FilterCaps, caps.InputFormatCaps,
                caps.AutoStreamCaps, caps.StereoCaps, caps.RateConversionCapsCount,
                caps.MaxInputStreams, caps.MaxStreamStates);
        for (i = 0; i < sizeof formats / sizeof formats[0]; i++)
            say("format %s (%d): %x", formats[i].name, (int)formats[i].f,
                format_support(vpe, formats[i].f));
        RELEASE(vpe);
    }
    {
        const struct probe probes[] = {
            {"32x32 to 64x64", 32, 32, 1, 1, 0, 0, bgra, 64, 64, 1, 0, bgra},
            {"same, again", 32, 32, 1, 1, 0, 0, bgra, 64, 64, 1, 0, bgra},
            {"48x40 to 80x72", 48, 40, 1, 1, 0, 0, bgra, 80, 72, 1, 0, bgra},
            {"source rect 4,6-20,30", 32, 32, 1, 1, 0, 0, bgra, 64, 64, 1, 0, bgra, &src},
            {"dest rect 8,10-56,50", 32, 32, 1, 1, 0, 0, bgra, 64, 64, 1, 0, bgra, NULL, &dst},
            {"target rect 2,3-60,61", 32, 32, 1, 1, 0, 0, bgra, 64, 64, 1, 0, bgra, NULL, NULL,
             &target},
            {"all three rects", 32, 32, 1, 1, 0, 0, bgra, 64, 64, 1, 0, bgra, &src, &dst, &target},
            {"background 0.25,0.5,0.75,1 with dest rect", 32, 32, 1, 1, 0, 0, bgra, 64, 64, 1, 0,
             bgra, NULL, &dst, NULL, &bg},
            {"background YCbCr 0.5,0.25,0.75,1 with dest rect", 32, 32, 1, 1, 0, 0, bgra, 64, 64,
             1, 0, bgra, NULL, &dst, NULL, &bg_ycbcr, TRUE},
            {"stream alpha 0.5", 32, 32, 1, 1, 0, 0, bgra, 64, 64, 1, 0, bgra, NULL, NULL, NULL,
             NULL, FALSE, 0.5f},
            {"rotation 90", 32, 32, 1, 1, 0, 0, bgra, 64, 64, 1, 0, bgra, NULL, NULL, NULL, NULL,
             FALSE, 0, D3D11_VIDEO_PROCESSOR_ROTATION_90},
            {"rotation 180", 32, 32, 1, 1, 0, 0, bgra, 64, 64, 1, 0, bgra, NULL, NULL, NULL, NULL,
             FALSE, 0, D3D11_VIDEO_PROCESSOR_ROTATION_180},
            {"input video 709 limited, output RGB limited", 32, 32, 1, 1, 0, 0, bgra, 64, 64, 1,
             0, bgra, NULL, NULL, NULL, NULL, FALSE, 0, 0, &video, &limited},
            {"frame format interlaced top first", 32, 32, 1, 1, 0, 0, bgra, 64, 64, 1, 0, bgra,
             NULL, NULL, NULL, NULL, FALSE, 0, 0, NULL, NULL,
             D3D11_VIDEO_FRAME_FORMAT_INTERLACED_TOP_FIELD_FIRST},
            {"alpha fill mode source stream", 32, 32, 1, 1, 0, 0, bgra, 64, 64, 1, 0, bgra, NULL,
             NULL, NULL, NULL, FALSE, 0, 0, NULL, NULL, 0,
             D3D11_VIDEO_PROCESSOR_ALPHA_FILL_MODE_SOURCE_STREAM},
            {"source mip 1 of 2, slice 1 of 3", 32, 32, 2, 3, 1, 1, bgra, 64, 64, 1, 0, bgra},
            {"target slice 2 of 3", 32, 32, 1, 1, 0, 0, bgra, 64, 64, 3, 2, bgra},
            {"R8G8B8A8 target", 32, 32, 1, 1, 0, 0, bgra, 64, 64, 1, 0,
             DXGI_FORMAT_R8G8B8A8_UNORM},
            {"NV12 source 32x32", 32, 32, 1, 1, 0, 0, DXGI_FORMAT_NV12, 64, 64, 1, 0, bgra},
            {"YUY2 source 32x32", 32, 32, 1, 1, 0, 0, DXGI_FORMAT_YUY2, 64, 64, 1, 0, bgra},
            {"two streams, second at 40,40-56,56", 32, 32, 1, 1, 0, 0, bgra, 64, 64, 1, 0, bgra,
             NULL, NULL, NULL, NULL, FALSE, 0, 0, NULL, NULL, 0, 0, 1},
        };
        for (i = 0; i < sizeof probes / sizeof probes[0]; i++) {
            HRESULT removed;
            run(n++, &probes[i]);
            removed = ID3D11Device_GetDeviceRemovedReason(dev);
            if (removed != S_OK) {
                say("device removed: 0x%08lx", (unsigned long)removed);
                break;
            }
        }
    }
    ID3D11DeviceContext_Flush(ctx);
    say("--- end");
    RELEASE(vctx);
    RELEASE(vdev);
    RELEASE(ctx);
    RELEASE(dev);
    log_close();
    return 0;
}
