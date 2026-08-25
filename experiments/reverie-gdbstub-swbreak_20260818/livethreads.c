#include <pthread.h>
#include <stdio.h>
#include <unistd.h>

volatile int global = 42;
static int pipefd[2];

static void *worker(void *a) { char c; read(pipefd[0], &c, 1); return 0; }

__attribute__((noinline)) void bkpt(void) { }

int main(void) {
    setvbuf(stdout, NULL, _IOLBF, 0);
    if (pipe(pipefd) != 0) return 1;
    pthread_t t[2];
    for (int i = 0; i < 2; i++) pthread_create(&t[i], NULL, worker, NULL);
    sleep(1);            /* let both workers block in read() */
    bkpt();              /* stop here with the workers alive and blocked */
    global = 43;
    write(pipefd[1], "xx", 2);
    for (int i = 0; i < 2; i++) pthread_join(t[i], NULL);
    printf("done %d\n", global);
    return 0;
}
