/*
 * modes: changes the display resolution of the guest, as resizing the
 * rsvga window does through the VMware Tools: 1280x720, then 1024x768,
 * then back to the starting mode, a few seconds each. Each change is
 * reported to the host (guest/common/hostlog.h) with the mode Windows
 * reads back; the screen captures show whether the desktop followed.
 *
 * Usage: modes [seconds per mode, default 4]
 * Build: scripts/build-guest-tools.sh (mingw-w64).
 */
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <stdlib.h>

#include "../common/hostlog.h"

static DEVMODEA current(void)
{
    DEVMODEA m;

    memset(&m, 0, sizeof m);
    m.dmSize = sizeof m;
    EnumDisplaySettingsA(NULL, ENUM_CURRENT_SETTINGS, &m);
    return m;
}

static int set_mode(DWORD width, DWORD height, int seconds)
{
    DEVMODEA m = current();
    LONG result;

    m.dmPelsWidth = width;
    m.dmPelsHeight = height;
    m.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT;
    result = ChangeDisplaySettingsExA(NULL, &m, NULL, 0, NULL);
    Sleep((DWORD)seconds * 1000);
    m = current();
    say("%lux%lu: result %ld, now %lux%lu at %lu bpp", (unsigned long)width,
        (unsigned long)height, (long)result, (unsigned long)m.dmPelsWidth,
        (unsigned long)m.dmPelsHeight, (unsigned long)m.dmBitsPerPel);
    return result == DISP_CHANGE_SUCCESSFUL && m.dmPelsWidth == width && m.dmPelsHeight == height;
}

int main(int argc, char **argv)
{
    int seconds = argc > 1 ? atoi(argv[1]) : 4;
    DEVMODEA start;
    int good = 0;

    log_open("modes");
    start = current();
    say("start: %lux%lu", (unsigned long)start.dmPelsWidth, (unsigned long)start.dmPelsHeight);
    good += set_mode(1280, 720, seconds);
    good += set_mode(1024, 768, seconds);
    good += set_mode(start.dmPelsWidth, start.dmPelsHeight, seconds);
    say("summary: %d of 3 modes set", good);
    say("--- end");
    log_close();
    return good == 3 ? 0 : 1;
}
