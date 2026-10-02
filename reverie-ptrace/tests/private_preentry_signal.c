/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */
#define _GNU_SOURCE
#include <signal.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

static volatile sig_atomic_t delivered;
static void handler(int signal) {
    if (signal != SIGUSR1) _exit(83);
    ++delivered;
}

int main(int argc, char **argv) {
    if (argc != 2) return 73;
    int signal = strcmp(argv[1], "signal") == 0;
    if (!signal && strcmp(argv[1], "ordinary") != 0) return 74;
    struct sigaction action = {0};
    action.sa_handler = handler;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGUSR1, &action, 0)) return 84;
    long result = syscall(SYS_write, 784, 0, 0);
    if (result != getpid() || delivered != signal) return 85;
    static const char marker[] = "private-returned\n";
    if (write(1, marker, sizeof(marker) - 1) != (ssize_t)(sizeof(marker) - 1)) return 86;
    return 0;
}
