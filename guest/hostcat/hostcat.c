/*
 * hostcat: sends a text file to the host, one line per report
 * (guest/common/hostlog.h), e.g. the report of `dxdiag /t`. UTF-16 files
 * (with their byte order mark) are converted to UTF-8; in other files,
 * zero bytes are dropped (UTF-16 parts appended by other commands).
 *
 * Usage: hostcat FILE
 * Build: scripts/build-guest-tools.sh (mingw-w64).
 */
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <stdlib.h>

#include "../common/hostlog.h"

static void send_lines(char *text)
{
    char *line = text;

    while (line && *line) {
        char *next = strchr(line, '\n');
        size_t len;

        if (next)
            *next++ = '\0';
        len = strlen(line);
        if (len && line[len - 1] == '\r')
            line[len - 1] = '\0';
        if (*line)
            say("%s", line);
        line = next;
    }
}

int main(int argc, char **argv)
{
    FILE *f;
    long size;
    unsigned char *data;
    char *text;

    if (argc < 2) {
        fprintf(stderr, "usage: hostcat FILE\n");
        return 2;
    }
    log_open("hostcat");
    f = fopen(argv[1], "rb");
    if (!f) {
        say("cannot open %s", argv[1]);
        log_close();
        return 1;
    }
    fseek(f, 0, SEEK_END);
    size = ftell(f);
    fseek(f, 0, SEEK_SET);
    data = calloc((size_t)size + 2, 1);
    if (!data || fread(data, 1, (size_t)size, f) != (size_t)size) {
        say("cannot read %s", argv[1]);
        log_close();
        return 1;
    }
    fclose(f);
    if (size >= 2 && data[0] == 0xff && data[1] == 0xfe) {
        int n = WideCharToMultiByte(CP_UTF8, 0, (const WCHAR *)(data + 2),
                                    (int)(size - 2) / 2, NULL, 0, NULL, NULL);
        text = calloc((size_t)n + 1, 1);
        WideCharToMultiByte(CP_UTF8, 0, (const WCHAR *)(data + 2), (int)(size - 2) / 2,
                            text, n, NULL, NULL);
    } else {
        /* Mixed files (several commands appending, some in UTF-16
         * without a byte order mark): keep the ASCII, drop zero bytes
         * and stray marks. */
        long i, n = 0;
        for (i = 0; i < size; i++) {
            if (data[i] == 0)
                continue;
            if (i + 1 < size && data[i] == 0xff && data[i + 1] == 0xfe) {
                i++;
                continue;
            }
            data[n++] = data[i];
        }
        data[n] = 0;
        text = (char *)data;
    }
    say("--- %s", argv[1]);
    send_lines(text);
    say("--- end");
    log_close();
    return 0;
}
