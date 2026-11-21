#pragma once

#include <stddef.h>

#define EXIT_SUCCESS 0
#define EXIT_FAILURE 1

void *malloc(size_t size);
void *calloc(size_t item_count, size_t item_size);
void *realloc(void *pointer, size_t size);
void free(void *pointer);

long strtol(const char *text, char **end, int base);
double strtod(const char *text, char **end);

static inline int atoi(const char *text)
{
    return (int)strtol(text, NULL, 10);
}

static inline double atof(const char *text)
{
    return strtod(text, NULL);
}

static inline int abs(int value)
{
    return value < 0 ? -value : value;
}

void exit(int code) __attribute__((noreturn));
int system(const char *command);
