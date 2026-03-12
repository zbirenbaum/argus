/*
 * Validation test: concurrent write interleaving.
 *
 * Four threads each write 100 times to the same file with O_TRUNC.
 * Under ptrace with per-path write serialization, every write event
 * should chain: event[N+1].before_hash == event[N].after_hash.
 *
 * Compile: gcc -O0 -pthread -o concurrent_write concurrent_write.c
 */

#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

/* Iterations per thread — high enough to trigger contention. */
#define ITERS 100
#define NUM_THREADS 4

static const char *target_path = "/tmp/argus-test-workspace/shared.txt";

static void *writer(void *arg) {
    int id = *(int *)arg;
    char buf[64];
    for (int i = 0; i < ITERS; i++) {
        int len = snprintf(buf, sizeof(buf), "writer %d iteration %d\n", id, i);
        int fd = open(target_path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
        if (fd < 0) {
            perror("open");
            return NULL;
        }
        write(fd, buf, (size_t)len);
        close(fd);
    }
    return NULL;
}

int main(void) {
    pthread_t threads[NUM_THREADS];
    int ids[NUM_THREADS];

    for (int i = 0; i < NUM_THREADS; i++) {
        ids[i] = i;
        if (pthread_create(&threads[i], NULL, writer, &ids[i]) != 0) {
            perror("pthread_create");
            return 1;
        }
    }

    for (int i = 0; i < NUM_THREADS; i++)
        pthread_join(threads[i], NULL);

    printf("done: %d threads x %d iterations = %d writes\n",
           NUM_THREADS, ITERS, NUM_THREADS * ITERS);
    return 0;
}
