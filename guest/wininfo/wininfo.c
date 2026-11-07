/*
 * wininfo: lists the visible top-level windows to the host
 * (guest/common/hostlog.h): process, class, title, size and whether the
 * window is layered, to tell which program owns what DWM composes.
 *
 * Build: scripts/build-guest-tools.sh (mingw-w64).
 */
#define WIN32_LEAN_AND_MEAN
#include <windows.h>

#include "../common/hostlog.h"

static BOOL CALLBACK one(HWND hwnd, LPARAM unused)
{
    char title[128] = "", cls[64] = "", exe[MAX_PATH] = "?";
    DWORD pid = 0, size = sizeof exe;
    LONG ex = GetWindowLongA(hwnd, GWL_EXSTYLE);
    HANDLE process;
    RECT r;
    const char *name;

    (void)unused;
    if (!IsWindowVisible(hwnd))
        return TRUE;
    GetWindowTextA(hwnd, title, sizeof title);
    GetClassNameA(hwnd, cls, sizeof cls);
    GetWindowRect(hwnd, &r);
    GetWindowThreadProcessId(hwnd, &pid);
    process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
    if (process) {
        QueryFullProcessImageNameA(process, 0, exe, &size);
        CloseHandle(process);
    }
    name = strrchr(exe, '\\') ? strrchr(exe, '\\') + 1 : exe;
    say("%s pid %lu: %ldx%ld at %ld,%ld%s%s class \"%s\" title \"%s\"", name,
        (unsigned long)pid, r.right - r.left, r.bottom - r.top, r.left, r.top,
        ex & WS_EX_LAYERED ? " layered" : "", ex & WS_EX_NOREDIRECTIONBITMAP ? " noredirect" : "",
        cls, title);
    return TRUE;
}

int main(void)
{
    log_open("wininfo");
    EnumWindows(one, 0);
    say("--- end");
    log_close();
    return 0;
}
