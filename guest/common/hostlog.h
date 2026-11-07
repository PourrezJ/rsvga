/*
 * Reporting to the host for the guest test programs: each line goes to
 * stdout, to a log file next to the program, and through the VMware
 * backdoor as a GuestRPC "log" message, which QEMU traces with the rsvga
 * vmport patch (run-windows.sh --diagnose: target/vm/qemu.log). The
 * backdoor works even when the guest shows nothing.
 *
 * Call log_open(name) first; say() then reports one line.
 */
#ifndef RSVGA_HOSTLOG_H
#define RSVGA_HOSTLOG_H

#include <windows.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

/* ---- VMware backdoor: GuestRPC (RPCI channel) ------------------------ */

#define BDOOR_MAGIC 0x564D5868u
#define BDOOR_PORT 0x5658u
#define BDOOR_CMD_MESSAGE 30u
#define RPCI_PROTOCOL 0x49435052u
#define RPC_FLAG_COOKIE 0x80000000u
#define RPC_SUCCESS 0x0001u
#define RPC_DORECV 0x0002u

enum { RPC_OPEN, RPC_SENDSIZE, RPC_SENDPAYLOAD, RPC_RECVSIZE,
       RPC_RECVPAYLOAD, RPC_RECVSTATUS, RPC_CLOSE };

struct regs { uint32_t ax, bx, cx, dx, si, di; };

/* One backdoor call. Ring 3 is fine: the hypervisor traps the port. */
static void bdoor(struct regs *r)
{
    __asm__ __volatile__("inl %%dx, %%eax"
                         : "+a"(r->ax), "+b"(r->bx), "+c"(r->cx),
                           "+d"(r->dx), "+S"(r->si), "+D"(r->di)
                         :
                         : "memory");
}

static int have_backdoor = 1;
/* Set while this thread runs a backdoor call: only then may a fault be
 * the hypervisor refusing the port. */
static __thread int in_backdoor;

static void rpc_step(struct regs *r, uint32_t type, uint32_t channel,
                     uint32_t bx, uint32_t hi, uint32_t lo)
{
    r->ax = BDOOR_MAGIC;
    r->bx = bx;
    r->cx = BDOOR_CMD_MESSAGE | type << 16;
    r->dx = channel << 16 | BDOOR_PORT;
    r->si = hi;
    r->di = lo;
    in_backdoor = 1;
    if (have_backdoor)
        bdoor(r);
    in_backdoor = 0;
}

/* Sends `msg` and discards the reply. Returns 0 if the host is absent. */
static int rpc_send(const char *msg)
{
    struct regs r = {0};
    uint32_t len = (uint32_t)strlen(msg), channel, hi, lo, i;

    if (!have_backdoor)
        return 0;
    rpc_step(&r, RPC_OPEN, 0, RPCI_PROTOCOL | RPC_FLAG_COOKIE, 0, 0);
    if (!(r.cx >> 16 & RPC_SUCCESS))
        return 0;
    channel = r.dx >> 16;
    hi = r.si;
    lo = r.di;
    rpc_step(&r, RPC_SENDSIZE, channel, len, hi, lo);
    for (i = 0; i < len; i += 4) {
        uint32_t word = 0;
        memcpy(&word, msg + i, len - i < 4 ? len - i : 4);
        rpc_step(&r, RPC_SENDPAYLOAD, channel, word, hi, lo);
    }
    rpc_step(&r, RPC_RECVSIZE, channel, 0, hi, lo);
    if (r.cx >> 16 & RPC_DORECV) {
        uint32_t size = r.bx;
        for (i = 0; i < size; i += 4)
            rpc_step(&r, RPC_RECVPAYLOAD, channel, RPC_SUCCESS, hi, lo);
        rpc_step(&r, RPC_RECVSTATUS, channel, RPC_SUCCESS, hi, lo);
    }
    rpc_step(&r, RPC_CLOSE, channel, 0, hi, lo);
    return 1;
}

/* ---- output ------------------------------------------------------------ */

static FILE *logfile;
static const char *log_name = "guest";

static void say(const char *fmt, ...)
{
    char line[1024];
    char msg[1100];
    va_list ap;

    va_start(ap, fmt);
    vsnprintf(line, sizeof line, fmt, ap);
    va_end(ap);
    printf("%s\n", line);
    if (logfile) {
        fprintf(logfile, "%s\n", line);
        fflush(logfile);
    }
    snprintf(msg, sizeof msg, "log %s: %s", log_name, line);
    rpc_send(msg);
}

static LONG WINAPI no_backdoor(EXCEPTION_POINTERS *e)
{
    /* A #GP on the backdoor port: not a VMware-compatible hypervisor.
     * Faults elsewhere belong to their own handlers. */
    if (in_backdoor &&
        (e->ExceptionRecord->ExceptionCode == EXCEPTION_PRIV_INSTRUCTION ||
         e->ExceptionRecord->ExceptionCode == EXCEPTION_ACCESS_VIOLATION)) {
        have_backdoor = 0;
        e->ContextRecord->Rip += 1; /* skip the one-byte `in` */
        return EXCEPTION_CONTINUE_EXECUTION;
    }
    return EXCEPTION_CONTINUE_SEARCH;
}

/* OutputDebugString messages of this process (the Direct3D runtimes and
 * debug drivers write there), reported when no debugger is attached. */
static inline LONG WINAPI debug_output(EXCEPTION_POINTERS *e)
{
    const EXCEPTION_RECORD *r = e->ExceptionRecord;
    const char *text = NULL;
    char line[1024];
    size_t len;

    if (r->ExceptionCode == 0x40010006 && r->NumberParameters >= 2) /* DBG_PRINTEXCEPTION_C */
        text = (const char *)r->ExceptionInformation[1];
    else if (r->ExceptionCode == 0x4001000A && r->NumberParameters >= 4) /* _WIDE_C */
        text = (const char *)r->ExceptionInformation[3];
    else
        return EXCEPTION_CONTINUE_SEARCH;
    if (text) {
        snprintf(line, sizeof line, "%s", text);
        len = strlen(line);
        while (len && (line[len - 1] == '\n' || line[len - 1] == '\r'))
            line[--len] = '\0';
        if (len)
            say("debug: %s", line);
    }
    return EXCEPTION_CONTINUE_EXECUTION;
}

/* Reports this process's OutputDebugString messages too. */
static inline void log_debug_output(void)
{
    AddVectoredExceptionHandler(0, debug_output);
}

/* Starts reporting as `name`, to `name`.log next to the program. */
static void log_open(const char *name)
{
    char path[MAX_PATH];
    char *slash;

    log_name = name;
    AddVectoredExceptionHandler(1, no_backdoor);
    GetModuleFileNameA(NULL, path, sizeof path);
    slash = strrchr(path, '\\');
    if (slash) {
        snprintf(slash + 1, sizeof path - (size_t)(slash + 1 - path), "%s.log", name);
        logfile = fopen(path, "a");
    }
}

static void log_close(void)
{
    if (logfile)
        fclose(logfile);
    logfile = NULL;
}

#endif
