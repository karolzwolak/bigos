#pragma once

#include <stdarg.h>
#include <stddef.h>

typedef struct bigc_file FILE;

extern FILE *stdout;
extern FILE *stderr;

#define EOF (-1)
#define SEEK_SET 0
#define SEEK_CUR 1
#define SEEK_END 2

FILE *fopen(const char *path, const char *mode);
int fclose(FILE *stream);
size_t fread(void *buffer, size_t item_size, size_t item_count, FILE *stream);
size_t fwrite(const void *buffer, size_t item_size, size_t item_count, FILE *stream);
int fseek(FILE *stream, long offset, int origin);
long ftell(FILE *stream);
int fflush(FILE *stream);
int fputc(int character, FILE *stream);
int putc(int character, FILE *stream);
int putchar(int character);
int puts(const char *text);

int printf(const char *format, ...) __attribute__((format(printf, 1, 2)));
int fprintf(FILE *stream, const char *format, ...) __attribute__((format(printf, 2, 3)));
int vfprintf(FILE *stream, const char *format, va_list arguments);
int snprintf(char *buffer, size_t capacity, const char *format, ...)
    __attribute__((format(printf, 3, 4)));
int vsnprintf(char *buffer, size_t capacity, const char *format, va_list arguments);
int sscanf(const char *input, const char *format, ...);

int remove(const char *path);
int rename(const char *old_path, const char *new_path);
