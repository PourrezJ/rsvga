/*
 * d3d9trace: what Windows' Direct3D 9 runtime asks the display driver,
 * and what it answers, reported to the host (guest/common/hostlog.h).
 *
 * The runtime finds the user-mode driver's OpenAdapter with
 * GetProcAddress: d3d9.dll's import of it is redirected here, so its
 * OpenAdapter becomes a wrapper that logs the call and wraps, in turn, the
 * driver's GetCaps and CreateDevice and the runtime's adapter-info
 * callback. Its D3DKMT* imports (the kernel's graphics interface) are
 * wrapped too. Then the usual queries: Direct3DCreate9,
 * GetAdapterIdentifier, GetDeviceCaps(HAL), CheckDeviceType.
 *
 * Build: scripts/build-guest-tools.sh (mingw-w64).
 */
#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d9.h>

#include "../common/hostlog.h"

/* ---- d3dumddi.h, as much as needed ------------------------------------ */

typedef struct {
    VOID *pPrivateDriverData;
    UINT PrivateDriverDataSize;
} D3DDDICB_QUERYADAPTERINFO;

typedef HRESULT(APIENTRY *PFN_QUERYADAPTERINFOCB)(HANDLE, const D3DDDICB_QUERYADAPTERINFO *);

typedef struct {
    PFN_QUERYADAPTERINFOCB pfnQueryAdapterInfoCb;
    void *pfnGetMultisampleMethodListCb;
    void *more[8]; /* newer runtimes may pass more callbacks */
} ADAPTERCALLBACKS;

typedef struct {
    UINT Type;
    VOID *pInfo;
    VOID *pData;
    UINT DataSize;
} D3DDDIARG_GETCAPS;

typedef HRESULT(APIENTRY *PFN_GETCAPS)(HANDLE, const D3DDDIARG_GETCAPS *);
typedef HRESULT(APIENTRY *PFN_CREATEDEVICE)(HANDLE, void *);

typedef struct {
    PFN_GETCAPS pfnGetCaps;
    PFN_CREATEDEVICE pfnCreateDevice;
    HRESULT(APIENTRY *pfnCloseAdapter)(HANDLE);
} ADAPTERFUNCS;

typedef struct {
    HANDLE hAdapter;
    UINT Interface;
    UINT Version;
    const ADAPTERCALLBACKS *pAdapterCallbacks;
    ADAPTERFUNCS *pAdapterFuncs;
    UINT DriverVersion;
} OPENADAPTER;

typedef HRESULT(APIENTRY *PFN_OPENADAPTER)(OPENADAPTER *);

/* ---- the wrappers ------------------------------------------------------- */

static FARPROC(WINAPI *real_get_proc_address)(HMODULE, LPCSTR);
static PFN_OPENADAPTER real_open_adapter;
static PFN_GETCAPS real_get_caps;
static PFN_CREATEDEVICE real_create_device;
static PFN_QUERYADAPTERINFOCB real_query_cb;
static ADAPTERCALLBACKS callbacks_copy;

static HRESULT APIENTRY query_cb(HANDLE h, const D3DDDICB_QUERYADAPTERINFO *q)
{
    HRESULT hr = real_query_cb(h, q);
    say("  runtime callback QueryAdapterInfo(%u bytes): hr=0x%08lx", q->PrivateDriverDataSize,
        (unsigned long)hr);
    return hr;
}

static HRESULT APIENTRY get_caps(HANDLE h, const D3DDDIARG_GETCAPS *a)
{
    HRESULT hr = real_get_caps(h, a);
    const UINT *d = a->pData;
    UINT info = a->pInfo ? *(const UINT *)a->pInfo : 0;

    if (d && a->DataSize >= 16)
        say("  GetCaps(type %u, info %u, %u bytes): hr=0x%08lx data %08x %08x %08x %08x", a->Type,
            info, a->DataSize, (unsigned long)hr, d[0], d[1], d[2], d[3]);
    else if (d && a->DataSize >= 4)
        say("  GetCaps(type %u, info %u, %u bytes): hr=0x%08lx data %08x", a->Type, info,
            a->DataSize, (unsigned long)hr, d[0]);
    else
        say("  GetCaps(type %u, info %u, %u bytes): hr=0x%08lx", a->Type, info, a->DataSize,
            (unsigned long)hr);
    return hr;
}

static HRESULT APIENTRY create_device(HANDLE h, void *a)
{
    HRESULT hr = real_create_device(h, a);
    say("  CreateDevice: hr=0x%08lx", (unsigned long)hr);
    return hr;
}

static HRESULT APIENTRY open_adapter(OPENADAPTER *a)
{
    HRESULT hr;

    say("OpenAdapter(interface %u, version 0x%x)", a->Interface, a->Version);
    if (a->pAdapterCallbacks) {
        callbacks_copy = *a->pAdapterCallbacks;
        real_query_cb = callbacks_copy.pfnQueryAdapterInfoCb;
        callbacks_copy.pfnQueryAdapterInfoCb = query_cb;
        a->pAdapterCallbacks = &callbacks_copy;
    }
    hr = real_open_adapter(a);
    say("OpenAdapter: hr=0x%08lx driver version 0x%x", (unsigned long)hr, a->DriverVersion);
    if (SUCCEEDED(hr) && a->pAdapterFuncs) {
        real_get_caps = a->pAdapterFuncs->pfnGetCaps;
        real_create_device = a->pAdapterFuncs->pfnCreateDevice;
        a->pAdapterFuncs->pfnGetCaps = get_caps;
        a->pAdapterFuncs->pfnCreateDevice = create_device;
    }
    return hr;
}

static FARPROC WINAPI get_proc_address(HMODULE module, LPCSTR name)
{
    FARPROC f = real_get_proc_address(module, name);
    char path[MAX_PATH] = "";

    GetModuleFileNameA(module, path, sizeof path);
    if ((UINT_PTR)name > 0xffff && (strstr(path, "vm3d") || strstr(path, "um")))
        say("runtime asks %s for %s: %p", strrchr(path, '\\') ? strrchr(path, '\\') + 1 : path,
            name, (void *)f);
    if ((UINT_PTR)name > 0xffff && f && strcmp(name, "OpenAdapter") == 0) {
        real_open_adapter = (PFN_OPENADAPTER)(void *)f;
        return (FARPROC)(void *)open_adapter;
    }
    return f;
}

/* ---- D3DKMT calls of the runtime ---------------------------------------- */

/* Every D3DKMT function takes one structure and returns an NTSTATUS. */
typedef LONG(APIENTRY *KMT)(void *);

#define KMT_SLOTS 96
static KMT kmt_real[KMT_SLOTS];
static char kmt_name[KMT_SLOTS][64];

/* Experiment (argument "rewrite"): the display modes the kernel lists in
 * A8B8G8R8 are handed to the runtime as A8R8G8B8. */
static int rewrite_modes;

static LONG kmt_call(int i, void *arg)
{
    LONG status = kmt_real[i](arg);
    if (rewrite_modes && status == 0 && strcmp(kmt_name[i], "D3DKMTGetDisplayModeList") == 0) {
        const UINT *a = arg;
        BYTE *modes = *(BYTE *const *)(a + 2);
        UINT m, changed = 0;
        for (m = 0; modes && m < a[4]; m++) {
            UINT *d = (UINT *)(modes + 44 * m);
            if (d[2] == 32) {
                d[2] = 21;
                changed++;
            }
        }
        if (modes)
            say("rewrite: %u modes A8B8G8R8 -> A8R8G8B8", changed);
    }
    if (strcmp(kmt_name[i], "D3DKMTQueryAdapterInfo") == 0) {
        /* { hAdapter, Type, pPrivateDriverData, PrivateDriverDataSize } */
        const UINT *a = arg;
        const UINT *out = *(void *const *)(a + 2);
        UINT size = a[4];
        if (status == 0 && out && size >= 16)
            say("%s(type %u, %u bytes): 0x%08lx -> %08x %08x %08x %08x", kmt_name[i], a[1], size,
                (unsigned long)status, out[0], out[1], out[2], out[3]);
        else if (status == 0 && out && size >= 4)
            say("%s(type %u, %u bytes): 0x%08lx -> %08x", kmt_name[i], a[1], size,
                (unsigned long)status, out[0]);
        else
            say("%s(type %u, %u bytes): 0x%08lx", kmt_name[i], a[1], size, (unsigned long)status);
    } else if (strcmp(kmt_name[i], "D3DKMTGetDisplayModeList") == 0) {
        /* { hAdapter, VidPnSourceId, pModeList, ModeCount }; modes are
         * { Width, Height, Format, IntegerRefreshRate, ... }, 44 bytes on
         * Windows 11. */
        const UINT *a = arg;
        const BYTE *modes = *(const BYTE *const *)(a + 2);
        UINT count = a[4], m;
        char line[900] = "";
        DEVMODEA dm;
        say("%s(source %u): 0x%08lx, %u modes%s", kmt_name[i], a[1], (unsigned long)status, count,
            modes ? "" : " (count only)");
        memset(&dm, 0, sizeof dm);
        dm.dmSize = sizeof dm;
        EnumDisplaySettingsA(NULL, ENUM_CURRENT_SETTINGS, &dm);
        for (m = 0; status == 0 && modes && m < count; m++) {
            const UINT *d = (const UINT *)(modes + 44 * m);
            char one[48];
            if (d[0] == dm.dmPelsWidth && d[1] == dm.dmPelsHeight)
                say("  the current mode: %ux%u format %u %u Hz, rate %u/%u, scanline %u, "
                    "orientation %u, fixed output %u, flags 0x%x, word10 0x%x",
                    d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7], d[8], d[9], d[10]);
            snprintf(one, sizeof one, "%ux%u/%u ", d[0], d[1], d[2]);
            if (strlen(line) + strlen(one) >= sizeof line) {
                say("  %s", line);
                line[0] = 0;
            }
            strcat(line, one);
        }
        if (line[0])
            say("  %s", line);
    } else {
        say("%s: 0x%08lx", kmt_name[i], (unsigned long)status);
    }
    return status;
}

#define KMT_WRAPPER(i) \
    static LONG APIENTRY kmt_##i(void *arg) { return kmt_call(i, arg); }
KMT_WRAPPER(0)
KMT_WRAPPER(1)
KMT_WRAPPER(2)
KMT_WRAPPER(3)
KMT_WRAPPER(4)
KMT_WRAPPER(5)
KMT_WRAPPER(6)
KMT_WRAPPER(7)
KMT_WRAPPER(8)
KMT_WRAPPER(9)
KMT_WRAPPER(10)
KMT_WRAPPER(11)
KMT_WRAPPER(12)
KMT_WRAPPER(13)
KMT_WRAPPER(14)
KMT_WRAPPER(15)
KMT_WRAPPER(16)
KMT_WRAPPER(17)
KMT_WRAPPER(18)
KMT_WRAPPER(19)
KMT_WRAPPER(20)
KMT_WRAPPER(21)
KMT_WRAPPER(22)
KMT_WRAPPER(23)
KMT_WRAPPER(24)
KMT_WRAPPER(25)
KMT_WRAPPER(26)
KMT_WRAPPER(27)
KMT_WRAPPER(28)
KMT_WRAPPER(29)
KMT_WRAPPER(30)
KMT_WRAPPER(31)
KMT_WRAPPER(32)
KMT_WRAPPER(33)
KMT_WRAPPER(34)
KMT_WRAPPER(35)
KMT_WRAPPER(36)
KMT_WRAPPER(37)
KMT_WRAPPER(38)
KMT_WRAPPER(39)
KMT_WRAPPER(40)
KMT_WRAPPER(41)
KMT_WRAPPER(42)
KMT_WRAPPER(43)
KMT_WRAPPER(44)
KMT_WRAPPER(45)
KMT_WRAPPER(46)
KMT_WRAPPER(47)
KMT_WRAPPER(48)
KMT_WRAPPER(49)
KMT_WRAPPER(50)
KMT_WRAPPER(51)
KMT_WRAPPER(52)
KMT_WRAPPER(53)
KMT_WRAPPER(54)
KMT_WRAPPER(55)
KMT_WRAPPER(56)
KMT_WRAPPER(57)
KMT_WRAPPER(58)
KMT_WRAPPER(59)
KMT_WRAPPER(60)
KMT_WRAPPER(61)
KMT_WRAPPER(62)
KMT_WRAPPER(63)
KMT_WRAPPER(64)
KMT_WRAPPER(65)
KMT_WRAPPER(66)
KMT_WRAPPER(67)
KMT_WRAPPER(68)
KMT_WRAPPER(69)
KMT_WRAPPER(70)
KMT_WRAPPER(71)
KMT_WRAPPER(72)
KMT_WRAPPER(73)
KMT_WRAPPER(74)
KMT_WRAPPER(75)
KMT_WRAPPER(76)
KMT_WRAPPER(77)
KMT_WRAPPER(78)
KMT_WRAPPER(79)
KMT_WRAPPER(80)
KMT_WRAPPER(81)
KMT_WRAPPER(82)
KMT_WRAPPER(83)
KMT_WRAPPER(84)
KMT_WRAPPER(85)
KMT_WRAPPER(86)
KMT_WRAPPER(87)
KMT_WRAPPER(88)
KMT_WRAPPER(89)
KMT_WRAPPER(90)
KMT_WRAPPER(91)
KMT_WRAPPER(92)
KMT_WRAPPER(93)
KMT_WRAPPER(94)
KMT_WRAPPER(95)
static const KMT kmt_wrappers[KMT_SLOTS] = {
    kmt_0,
    kmt_1,
    kmt_2,
    kmt_3,
    kmt_4,
    kmt_5,
    kmt_6,
    kmt_7,
    kmt_8,
    kmt_9,
    kmt_10,
    kmt_11,
    kmt_12,
    kmt_13,
    kmt_14,
    kmt_15,
    kmt_16,
    kmt_17,
    kmt_18,
    kmt_19,
    kmt_20,
    kmt_21,
    kmt_22,
    kmt_23,
    kmt_24,
    kmt_25,
    kmt_26,
    kmt_27,
    kmt_28,
    kmt_29,
    kmt_30,
    kmt_31,
    kmt_32,
    kmt_33,
    kmt_34,
    kmt_35,
    kmt_36,
    kmt_37,
    kmt_38,
    kmt_39,
    kmt_40,
    kmt_41,
    kmt_42,
    kmt_43,
    kmt_44,
    kmt_45,
    kmt_46,
    kmt_47,
    kmt_48,
    kmt_49,
    kmt_50,
    kmt_51,
    kmt_52,
    kmt_53,
    kmt_54,
    kmt_55,
    kmt_56,
    kmt_57,
    kmt_58,
    kmt_59,
    kmt_60,
    kmt_61,
    kmt_62,
    kmt_63,
    kmt_64,
    kmt_65,
    kmt_66,
    kmt_67,
    kmt_68,
    kmt_69,
    kmt_70,
    kmt_71,
    kmt_72,
    kmt_73,
    kmt_74,
    kmt_75,
    kmt_76,
    kmt_77,
    kmt_78,
    kmt_79,
    kmt_80,
    kmt_81,
    kmt_82,
    kmt_83,
    kmt_84,
    kmt_85,
    kmt_86,
    kmt_87,
    kmt_88,
    kmt_89,
    kmt_90,
    kmt_91,
    kmt_92,
    kmt_93,
    kmt_94,
    kmt_95,
};

/* Wraps every D3DKMT* import of `module` (by name); returns how many. */
static int wrap_kmt_imports(HMODULE module)
{
    BYTE *base = (BYTE *)module;
    IMAGE_DOS_HEADER *dos = (IMAGE_DOS_HEADER *)base;
    IMAGE_NT_HEADERS *nt = (IMAGE_NT_HEADERS *)(base + dos->e_lfanew);
    IMAGE_DATA_DIRECTORY dir = nt->OptionalHeader.DataDirectory[IMAGE_DIRECTORY_ENTRY_IMPORT];
    IMAGE_IMPORT_DESCRIPTOR *imp = (IMAGE_IMPORT_DESCRIPTOR *)(base + dir.VirtualAddress);
    int n = 0;

    for (; dir.VirtualAddress && imp->Name; imp++) {
        IMAGE_THUNK_DATA *t = (IMAGE_THUNK_DATA *)(base + imp->FirstThunk);
        IMAGE_THUNK_DATA *names =
            (IMAGE_THUNK_DATA *)(base + (imp->OriginalFirstThunk ? imp->OriginalFirstThunk
                                                                 : imp->FirstThunk));
        for (; t->u1.Function && names->u1.AddressOfData; t++, names++) {
            const IMAGE_IMPORT_BY_NAME *by_name;
            DWORD old;
            if (IMAGE_SNAP_BY_ORDINAL(names->u1.Ordinal))
                continue;
            by_name = (const IMAGE_IMPORT_BY_NAME *)(base + names->u1.AddressOfData);
            if (strncmp((const char *)by_name->Name, "D3DKMT", 6) != 0 || n >= KMT_SLOTS)
                continue;
            snprintf(kmt_name[n], sizeof kmt_name[n], "%s", (const char *)by_name->Name);
            kmt_real[n] = (KMT)(void *)t->u1.Function;
            VirtualProtect(&t->u1.Function, sizeof t->u1.Function, PAGE_READWRITE, &old);
            t->u1.Function = (ULONG_PTR)(void *)kmt_wrappers[n];
            VirtualProtect(&t->u1.Function, sizeof t->u1.Function, old, &old);
            n++;
        }
    }
    return n;
}

/* Redirects `module`'s imports of `target` to `hook`; returns how many. */
static int patch_imports(HMODULE module, void *target, void *hook)
{
    BYTE *base = (BYTE *)module;
    IMAGE_DOS_HEADER *dos = (IMAGE_DOS_HEADER *)base;
    IMAGE_NT_HEADERS *nt = (IMAGE_NT_HEADERS *)(base + dos->e_lfanew);
    IMAGE_DATA_DIRECTORY dir = nt->OptionalHeader.DataDirectory[IMAGE_DIRECTORY_ENTRY_IMPORT];
    IMAGE_IMPORT_DESCRIPTOR *imp = (IMAGE_IMPORT_DESCRIPTOR *)(base + dir.VirtualAddress);
    int patched = 0;

    if (!dir.VirtualAddress)
        return 0;
    for (; imp->Name; imp++) {
        IMAGE_THUNK_DATA *t = (IMAGE_THUNK_DATA *)(base + imp->FirstThunk);
        for (; t->u1.Function; t++) {
            if ((void *)t->u1.Function == target) {
                DWORD old;
                VirtualProtect(&t->u1.Function, sizeof t->u1.Function, PAGE_READWRITE, &old);
                t->u1.Function = (ULONG_PTR)hook;
                VirtualProtect(&t->u1.Function, sizeof t->u1.Function, old, &old);
                patched++;
            }
        }
    }
    return patched;
}

int main(int argc, char **argv)
{
    HMODULE d3d9 = GetModuleHandleA("d3d9.dll");
    HMODULE kernelbase = GetModuleHandleA("kernelbase.dll");
    HMODULE kernel32 = GetModuleHandleA("kernel32.dll");
    void *gpa_base = kernelbase ? (void *)GetProcAddress(kernelbase, "GetProcAddress") : NULL;
    void *gpa_k32 = (void *)GetProcAddress(kernel32, "GetProcAddress");
    IDirect3D9 *d3d;
    D3DADAPTER_IDENTIFIER9 id;
    D3DCAPS9 caps;
    HRESULT hr;
    int n;

    log_open("d3d9trace");
    rewrite_modes = argc > 1 && strcmp(argv[1], "rewrite") == 0;
    log_debug_output();
    real_get_proc_address = (FARPROC(WINAPI *)(HMODULE, LPCSTR))gpa_k32;
    if (!d3d9)
        d3d9 = LoadLibraryA("d3d9.dll");
    n = patch_imports(d3d9, gpa_base, (void *)get_proc_address) +
        patch_imports(d3d9, gpa_k32, (void *)get_proc_address);
    say("d3d9.dll imports of GetProcAddress redirected: %d", n);
    n = wrap_kmt_imports(d3d9);
    say("d3d9.dll D3DKMT imports wrapped: %d", n);
    {
        char line[900] = "";
        int i;
        for (i = 0; i < n; i++) {
            if (strlen(line) + strlen(kmt_name[i]) + 2 >= sizeof line) {
                say("  %s", line);
                line[0] = 0;
            }
            strcat(line, kmt_name[i] + 6);
            strcat(line, " ");
        }
        say("  %s", line);
    }
    d3d = Direct3DCreate9(D3D_SDK_VERSION);
    if (!d3d) {
        say("Direct3DCreate9 failed");
        goto out;
    }
    memset(&id, 0, sizeof id);
    hr = IDirect3D9_GetAdapterIdentifier(d3d, 0, 0, &id);
    say("GetAdapterIdentifier: hr=0x%08lx driver %s", (unsigned long)hr, id.Driver);
    {
        D3DDISPLAYMODE mode;
        memset(&mode, 0, sizeof mode);
        hr = IDirect3D9_GetAdapterDisplayMode(d3d, 0, &mode);
        say("GetAdapterDisplayMode: hr=0x%08lx %ux%u format %u %u Hz", (unsigned long)hr,
            mode.Width, mode.Height, (unsigned)mode.Format, mode.RefreshRate);
        say("adapter modes: X8R8G8B8 %u, A8R8G8B8 %u, R5G6B5 %u, A2R10G10B10 %u",
            IDirect3D9_GetAdapterModeCount(d3d, 0, D3DFMT_X8R8G8B8),
            IDirect3D9_GetAdapterModeCount(d3d, 0, D3DFMT_A8R8G8B8),
            IDirect3D9_GetAdapterModeCount(d3d, 0, D3DFMT_R5G6B5),
            IDirect3D9_GetAdapterModeCount(d3d, 0, D3DFMT_A2R10G10B10));
    }
    memset(&caps, 0, sizeof caps);
    hr = IDirect3D9_GetDeviceCaps(d3d, 0, D3DDEVTYPE_HAL, &caps);
    say("GetDeviceCaps(HAL): hr=0x%08lx vs=0x%08lx ps=0x%08lx", (unsigned long)hr,
        (unsigned long)caps.VertexShaderVersion, (unsigned long)caps.PixelShaderVersion);
    hr = IDirect3D9_CheckDeviceType(d3d, 0, D3DDEVTYPE_HAL, D3DFMT_X8R8G8B8, D3DFMT_X8R8G8B8, TRUE);
    say("CheckDeviceType(HAL, X8R8G8B8, windowed): hr=0x%08lx", (unsigned long)hr);
    {
        DEVMODEA dm;
        memset(&dm, 0, sizeof dm);
        dm.dmSize = sizeof dm;
        EnumDisplaySettingsA(NULL, ENUM_CURRENT_SETTINGS, &dm);
        say("current desktop mode: %lux%lu %lu bpp %lu Hz", dm.dmPelsWidth, dm.dmPelsHeight,
            dm.dmBitsPerPel, dm.dmDisplayFrequency);
    }
    IDirect3D9_Release(d3d);
out:
    say("--- end");
    log_close();
    return 0;
}
