
#include <pthread.h>
#include <stdint.h>
static uint64_t read_tsc(void) {
    unsigned low, high;
    __asm__ volatile("rdtsc" : "=a"(low), "=d"(high));
    return ((uint64_t)high << 32) | low;
}
static void *worker(void *argument) {
    (void)read_tsc();
    return argument;
}
int main(void) {
    if (read_tsc() != UINT64_C(1234605616436508552)) return 10;
    pthread_t thread;
    void *result = 0;
    if (pthread_create(&thread, 0, worker, (void *)(uintptr_t)0x1234)) return 11;
    if (pthread_join(thread, &result)) return 12;
    if (result != (void *)(uintptr_t)0x1234) return 13;
    return read_tsc() == UINT64_C(1234605616436508552) ? 0 : 14;
}
