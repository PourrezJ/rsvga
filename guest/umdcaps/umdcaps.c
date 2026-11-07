/*
 * umdcaps: asks the display driver's Direct3D 9 user-mode driver directly
 * what Windows' Direct3D 9 runtime would get from it, and reports it to
 * the host (guest/common/hostlog.h).
 *
 * The runtime only says D3DERR_NOTAVAILABLE when it rejects a driver; this
 * loads the driver the kernel names for Direct3D 9, calls its OpenAdapter
 * the way the runtime does (its adapter queries go to the kernel through
 * D3DKMTQueryAdapterInfo), then GetCaps: the error code, the private data
 * the kernel driver gives it, its format list and its D3DCAPS9.
 *
 * The WDK's d3dumddi.h and d3dkmthk.h are not in mingw-w64: the few
 * structures needed are declared below with their documented layouts.
 *
 * Build: scripts/build-guest-tools.sh (mingw-w64).
 */
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d9.h>
#include <stdlib.h>

#include "../common/hostlog.h"

/* ---- d3dkmthk.h ------------------------------------------------------- */

typedef UINT D3DKMT_HANDLE;

typedef struct {
    WCHAR DeviceName[32];
    D3DKMT_HANDLE hAdapter;
    LUID AdapterLuid;
    UINT VidPnSourceId;
} D3DKMT_OPENADAPTERFROMGDIDISPLAYNAME;

typedef struct {
    D3DKMT_HANDLE hAdapter;
    UINT Type; /* KMTQUERYADAPTERINFOTYPE */
    VOID *pPrivateDriverData;
    UINT PrivateDriverDataSize;
} D3DKMT_QUERYADAPTERINFO;

enum { KMTQAITYPE_UMDRIVERPRIVATE = 0, KMTQAITYPE_UMDRIVERNAME = 1 };
enum { KMTUMDVERSION_DX9 = 0 };

typedef struct {
    UINT Version; /* KMTUMDVERSION */
    WCHAR UmdFileName[MAX_PATH];
} D3DKMT_UMDFILENAMEINFO;

typedef struct {
    UINT Count;
    UINT Quality;
} D3DDDI_MULTISAMPLINGMETHOD;

typedef struct {
    D3DKMT_HANDLE hAdapter;
    UINT VidPnSourceId;
    UINT Width;
    UINT Height;
    UINT Format;
    BOOL StereoEnabled;
    UINT MultisampleMethodCount;
    D3DDDI_MULTISAMPLINGMETHOD *pMultisampleMethods;
} D3DKMT_GETMULTISAMPLEMETHODLIST;

typedef LONG(APIENTRY *OPENFROMGDI)(D3DKMT_OPENADAPTERFROMGDIDISPLAYNAME *);
typedef LONG(APIENTRY *GETMSLIST)(D3DKMT_GETMULTISAMPLEMETHODLIST *);
typedef LONG(APIENTRY *QUERYADAPTERINFO)(const D3DKMT_QUERYADAPTERINFO *);

/* ---- d3dumddi.h ------------------------------------------------------- */

typedef struct {
    VOID *pPrivateDriverData;
    UINT PrivateDriverDataSize;
} D3DDDICB_QUERYADAPTERINFO;

typedef struct {
    UINT VidPnSourceId;
    UINT Width;
    UINT Height;
    UINT Format;
    UINT MethodCount;
    D3DDDI_MULTISAMPLINGMETHOD *pMethodList;
} D3DDDICB_GETMULTISAMPLEMETHODLIST;

typedef struct {
    UINT Format;
    BOOL bFlip;
    UINT MsType;
    UINT QualityLevels;
} DDIMULTISAMPLEQUALITYLEVELSDATA;

typedef HRESULT(APIENTRY *PFN_QUERYADAPTERINFOCB)(HANDLE, const D3DDDICB_QUERYADAPTERINFO *);
typedef HRESULT(APIENTRY *PFN_GETMULTISAMPLEMETHODLISTCB)(HANDLE, void *);

typedef struct {
    PFN_QUERYADAPTERINFOCB pfnQueryAdapterInfoCb;
    PFN_GETMULTISAMPLEMETHODLISTCB pfnGetMultisampleMethodListCb;
} D3DDDI_ADAPTERCALLBACKS;

typedef struct {
    UINT Type; /* D3DDDICAPS_TYPE */
    VOID *pInfo;
    VOID *pData;
    UINT DataSize;
} D3DDDIARG_GETCAPS;

typedef HRESULT(APIENTRY *PFN_GETCAPS)(HANDLE, const D3DDDIARG_GETCAPS *);

typedef struct {
    PFN_GETCAPS pfnGetCaps;
    void *pfnCreateDevice;
    HRESULT(APIENTRY *pfnCloseAdapter)(HANDLE);
} D3DDDI_ADAPTERFUNCS;

typedef struct {
    HANDLE hAdapter;
    UINT Interface;
    UINT Version;
    const D3DDDI_ADAPTERCALLBACKS *pAdapterCallbacks;
    D3DDDI_ADAPTERFUNCS *pAdapterFuncs;
    UINT DriverVersion;
} D3DDDIARG_OPENADAPTER;

typedef HRESULT(APIENTRY *PFN_OPENADAPTER)(D3DDDIARG_OPENADAPTER *);

enum {
    D3DDDICAPS_DDRAW = 1,
    D3DDDICAPS_GETFORMATCOUNT = 3,
    D3DDDICAPS_GETFORMATDATA = 4,
    D3DDDICAPS_GETD3D9CAPS = 13,
};

typedef struct {
    UINT Format; /* D3DDDIFORMAT, as D3DFORMAT */
    UINT Operations;
    UINT FlipMsTypes;
    UINT BltMsTypes;
    UINT PrivateFormatBitCount;
} FORMATOP;

/* ---- the runtime's side ------------------------------------------------- */

static const char *utf8(const WCHAR *w)
{
    static char out[512];
    WideCharToMultiByte(CP_UTF8, 0, w, -1, out, sizeof out, NULL, NULL);
    return out;
}

static QUERYADAPTERINFO query_adapter_info;
static D3DKMT_HANDLE kmt_adapter;

static HRESULT APIENTRY query_cb(HANDLE unused, const D3DDDICB_QUERYADAPTERINFO *q)
{
    D3DKMT_QUERYADAPTERINFO info = {kmt_adapter, KMTQAITYPE_UMDRIVERPRIVATE,
                                    q->pPrivateDriverData, q->PrivateDriverDataSize};
    LONG status = query_adapter_info(&info);
    UINT i;

    (void)unused;
    say("driver queries the adapter: %u bytes, status 0x%08lx", q->PrivateDriverDataSize,
        (unsigned long)status);
    /* The kernel driver's private data, 8 words per line. */
    for (i = 0; status == 0 && i + 32 <= q->PrivateDriverDataSize; i += 32) {
        const UINT *w = (const UINT *)((const char *)q->pPrivateDriverData + i);
        say("private %04x: %08x %08x %08x %08x %08x %08x %08x %08x", i, w[0], w[1], w[2], w[3],
            w[4], w[5], w[6], w[7]);
    }
    return status == 0 ? S_OK : E_FAIL;
}

static GETMSLIST get_ms_list;

/* The runtime asks the kernel; so do we. */
static HRESULT APIENTRY multisample_cb(HANDLE unused, void *arg)
{
    D3DDDICB_GETMULTISAMPLEMETHODLIST *l = arg;
    D3DKMT_GETMULTISAMPLEMETHODLIST k = {kmt_adapter, l->VidPnSourceId, l->Width, l->Height,
                                         l->Format, FALSE, l->MethodCount, l->pMethodList};
    LONG status = get_ms_list ? get_ms_list(&k) : -1;

    (void)unused;
    l->MethodCount = k.MultisampleMethodCount;
    say("driver asks the multisample methods of format %u %ux%u: status 0x%08lx, %u methods",
        l->Format, l->Width, l->Height, (unsigned long)status, k.MultisampleMethodCount);
    return status == 0 ? S_OK : (status == (LONG)0xC0000023 ? D3DERR_MOREDATA : E_FAIL);
}

static HRESULT caps(D3DDDI_ADAPTERFUNCS *funcs, HANDLE adapter, UINT type, void *data, UINT size)
{
    D3DDDIARG_GETCAPS arg = {type, NULL, data, size};
    return funcs->pfnGetCaps(adapter, &arg);
}

int main(void)
{
    static const UINT versions[] = {0x4002, 0x3004, 0x2003, 0x5002, 0xc000};
    D3DKMT_OPENADAPTERFROMGDIDISPLAYNAME open = {L"\\\\.\\DISPLAY1", 0, {0, 0}, 0};
    D3DKMT_UMDFILENAMEINFO name;
    D3DKMT_QUERYADAPTERINFO q;
    D3DDDI_ADAPTERCALLBACKS callbacks = {query_cb, multisample_cb};
    D3DDDI_ADAPTERFUNCS funcs;
    D3DDDIARG_OPENADAPTER arg;
    OPENFROMGDI open_from_gdi;
    PFN_OPENADAPTER open_adapter;
    HMODULE gdi, umd;
    HRESULT hr = E_FAIL;
    UINT count = 0, i, v;
    D3DCAPS9 d3d9;

    log_open("umdcaps");
    log_debug_output();
    gdi = LoadLibraryA("gdi32.dll");
    open_from_gdi = (OPENFROMGDI)(void *)GetProcAddress(gdi, "D3DKMTOpenAdapterFromGdiDisplayName");
    query_adapter_info = (QUERYADAPTERINFO)(void *)GetProcAddress(gdi, "D3DKMTQueryAdapterInfo");
    get_ms_list = (GETMSLIST)(void *)GetProcAddress(gdi, "D3DKMTGetMultisampleMethodList");
    if (!open_from_gdi || !query_adapter_info || open_from_gdi(&open) != 0) {
        say("cannot open the adapter of \\\\.\\DISPLAY1");
        goto out;
    }
    memset(&name, 0, sizeof name);
    name.Version = KMTUMDVERSION_DX9;
    q.hAdapter = open.hAdapter;
    q.Type = KMTQAITYPE_UMDRIVERNAME;
    q.pPrivateDriverData = &name;
    q.PrivateDriverDataSize = sizeof name;
    if (query_adapter_info(&q) != 0) {
        say("no Direct3D 9 driver name");
        goto out;
    }
    say("Direct3D 9 driver: %s", utf8(name.UmdFileName));
    kmt_adapter = open.hAdapter;
    umd = LoadLibraryW(name.UmdFileName);
    open_adapter = umd ? (PFN_OPENADAPTER)(void *)GetProcAddress(umd, "OpenAdapter") : NULL;
    if (!open_adapter) {
        say("cannot load it (error %lu)", GetLastError());
        goto out;
    }
    /* Interface 9: Direct3D 9. Each Windows passes its own interface
     * version (d3dumddi.h: Windows 7 0x2003 ... WDDM 3.x): a driver may
     * refuse versions newer than it knows. Try them all, keep the first
     * that works. */
    {
        static const UINT all[] = {0x2003, 0x3004, 0x4002, 0x5002, 0x6003, 0x7001, 0x8001,
                                   0x9006, 0xa00b, 0xb004, 0xc004, 0xd001, 0xe003, 0xf003,
                                   0x10002, 0x11001};
        UINT good = 0;
        for (v = 0; v < sizeof all / sizeof all[0]; v++) {
            memset(&funcs, 0, sizeof funcs);
            memset(&arg, 0, sizeof arg);
            arg.hAdapter = (HANDLE)(UINT_PTR)0x1234;
            arg.Interface = 9;
            arg.Version = all[v];
            arg.pAdapterCallbacks = &callbacks;
            arg.pAdapterFuncs = &funcs;
            hr = open_adapter(&arg);
            say("OpenAdapter(interface 9, version 0x%x): hr=0x%08lx, driver version 0x%x",
                all[v], (unsigned long)hr, arg.DriverVersion);
            if (SUCCEEDED(hr) && funcs.pfnCloseAdapter)
                funcs.pfnCloseAdapter(arg.hAdapter);
            if (SUCCEEDED(hr) && !good)
                good = all[v];
        }
        (void)versions;
        if (!good)
            goto out;
        memset(&funcs, 0, sizeof funcs);
        memset(&arg, 0, sizeof arg);
        arg.hAdapter = (HANDLE)(UINT_PTR)0x1234;
        arg.Interface = 9;
        arg.Version = good;
        arg.pAdapterCallbacks = &callbacks;
        arg.pAdapterFuncs = &funcs;
        hr = open_adapter(&arg);
    }
    if (FAILED(hr) || !funcs.pfnGetCaps)
        goto out;
    hr = caps(&funcs, arg.hAdapter, D3DDDICAPS_GETFORMATCOUNT, &count, sizeof count);
    say("format count: hr=0x%08lx %u", (unsigned long)hr, count);
    if (SUCCEEDED(hr) && count > 0 && count < 1024) {
        FORMATOP *ops = calloc(count, sizeof *ops);
        hr = caps(&funcs, arg.hAdapter, D3DDDICAPS_GETFORMATDATA, ops, count * sizeof *ops);
        for (i = 0; SUCCEEDED(hr) && i < count; i++)
            say("format %3u (0x%08x): ops 0x%08x flip ms 0x%x blt ms 0x%x", ops[i].Format,
                ops[i].Format, ops[i].Operations, ops[i].FlipMsTypes, ops[i].BltMsTypes);
        free(ops);
    }
    memset(&d3d9, 0, sizeof d3d9);
    hr = caps(&funcs, arg.hAdapter, D3DDDICAPS_GETD3D9CAPS, &d3d9, sizeof d3d9);
    say("D3D9 caps: hr=0x%08lx caps=0x%08lx caps2=0x%08lx devcaps=0x%08lx vs=0x%08lx ps=0x%08lx",
        (unsigned long)hr, d3d9.Caps, d3d9.Caps2, d3d9.DevCaps, d3d9.VertexShaderVersion,
        d3d9.PixelShaderVersion);
    say("D3D9 caps: devcaps2=0x%08lx max point size %f primitive misc=0x%08lx stencil=0x%08lx",
        d3d9.DevCaps2, d3d9.MaxPointSize, d3d9.PrimitiveMiscCaps, d3d9.StencilCaps);
    say("D3D9 caps: texture %lux%lu volume %lu raster=0x%08lx texture caps=0x%08lx "
        "max streams %lu max lights %lu",
        d3d9.MaxTextureWidth, d3d9.MaxTextureHeight, d3d9.MaxVolumeExtent, d3d9.RasterCaps,
        d3d9.TextureCaps, d3d9.MaxStreams, d3d9.MaxActiveLights);
    /* Every older caps query the runtime may make (types 1 to 12). */
    for (i = 1; i <= 12; i++) {
        static UINT data[1024], info[16];
        if (i == 3 || i == 4)
            continue;
        memset(data, 0, sizeof data);
        memset(info, 0, sizeof info);
        {
            D3DDDIARG_GETCAPS a = {i, info, data, sizeof data};
            hr = funcs.pfnGetCaps(arg.hAdapter, &a);
        }
        say("caps type %2u: hr=0x%08lx data %08x %08x %08x %08x", i, (unsigned long)hr, data[0],
            data[1], data[2], data[3]);
    }
    /* Multisample quality levels of the display format. */
    for (i = 2; i <= 8; i *= 2) {
        DDIMULTISAMPLEQUALITYLEVELSDATA ms = {D3DFMT_X8R8G8B8, FALSE, i, 0};
        D3DDDIARG_GETCAPS a = {5, &ms, &ms, sizeof ms};
        hr = funcs.pfnGetCaps(arg.hAdapter, &a);
        say("multisample %ux of X8R8G8B8: hr=0x%08lx quality levels %u", i, (unsigned long)hr,
            ms.QualityLevels);
    }
    if (funcs.pfnCloseAdapter)
        funcs.pfnCloseAdapter(arg.hAdapter);
out:
    say("--- end");
    log_close();
    return 0;
}
