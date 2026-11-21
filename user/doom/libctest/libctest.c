#include <errno.h>
#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>

void libctest_report(const char *name, int passed);
void bigc_set_preload_max_bytes(size_t limit_bytes);

#define CHECK(condition) libctest_report(#condition, (condition) != 0)
#define CHECK_STR(actual, expected) libctest_report(#actual " == " #expected, strcmp((actual), (expected)) == 0)

#define LONG_LINE_BYTES 700
#define PATTERN_PERIOD 251
#define PATTERN_FILE_BYTES 100000
#define SMALL_PRELOAD_LIMIT_BYTES 1000
#define DEFAULT_PRELOAD_LIMIT_BYTES (8 * 1024 * 1024)
#define MALLOC_ALIGN_BYTES 16
#define FILE_CYCLE_COUNT 20
#define EXIT_CHECK_CODE 3

static char scratch[256];

/* the byte at an index of BIG.BIN on the test image */
static unsigned char pattern_byte(size_t index)
{
    return (unsigned char)(index % PATTERN_PERIOD);
}

void libctest_fill_long_line(char *buffer)
{
    for (int index = 0; index < LONG_LINE_BYTES; index++) {
        buffer[index] = (char)('a' + index % 26);
    }
    buffer[LONG_LINE_BYTES] = '\0';
}

static void test_integer_formats(void)
{
    snprintf(scratch, sizeof scratch, "%d|%i|%d", -54, 17, 0);
    CHECK_STR(scratch, "-54|17|0");
    snprintf(scratch, sizeof scratch, "%5d|%-5d|%05d", 54, 54, 54);
    CHECK_STR(scratch, "   54|54   |00054");
    snprintf(scratch, sizeof scratch, "%+d|% d|%+d", 7, 7, -7);
    CHECK_STR(scratch, "+7| 7|-7");
    snprintf(scratch, sizeof scratch, "%u", 4294967295u);
    CHECK_STR(scratch, "4294967295");
    snprintf(scratch, sizeof scratch, "%x|%X|%#x|%#X|%#x", 255, 255, 255, 255, 0);
    CHECK_STR(scratch, "ff|FF|0xff|0XFF|0");
    snprintf(scratch, sizeof scratch, "%o|%#o|%#o", 8, 8, 0);
    CHECK_STR(scratch, "10|010|0");
    snprintf(scratch, sizeof scratch, "%.3d|%.0d|%6.3d|%-6.3d|", 5, 0, 5, 5);
    CHECK_STR(scratch, "005||   005|005   |");
    snprintf(scratch, sizeof scratch, "%ld|%lld|%zu", -5000000000L, -5000000000LL, (size_t)123456789012);
    CHECK_STR(scratch, "-5000000000|-5000000000|123456789012");
    snprintf(scratch, sizeof scratch, "%hhd|%hd|%lu", 300, 70000, 4000000000ul);
    CHECK_STR(scratch, "44|4464|4000000000");
    snprintf(scratch, sizeof scratch, "%*d|%-*d|", 6, 54, 6, 54);
    CHECK_STR(scratch, "    54|54    |");
    snprintf(scratch, sizeof scratch, "%d|%x", INT_MIN, 0xdeadbeefu);
    CHECK_STR(scratch, "-2147483648|deadbeef");
    snprintf(scratch, sizeof scratch, "%08x|%#010x", 0xabcu, 0xabcu);
    CHECK_STR(scratch, "00000abc|0x00000abc");
}

static void test_text_formats(void)
{
    snprintf(scratch, sizeof scratch, "%c%c|%5c|%-3c|", 'o', 'k', 'x', 'y');
    CHECK_STR(scratch, "ok|    x|y  |");
    snprintf(scratch, sizeof scratch, "%s|%5s|%-5s|%.1s|%.*s", "hi", "hi", "hi", "hi", 2, "abcdef");
    CHECK_STR(scratch, "hi|   hi|hi   |h|ab");
    snprintf(scratch, sizeof scratch, "%s|%%|%p", (const char *)NULL, (void *)0x1234);
    CHECK_STR(scratch, "(null)|%|0x1234");
    snprintf(scratch, sizeof scratch, "a%cb", 0);
    CHECK(scratch[0] == 'a' && scratch[1] == '\0' && scratch[2] == 'b');
}

static void test_float_formats(void)
{
    snprintf(scratch, sizeof scratch, "%.0f|%.0f|%.0f|%.0f", 0.5, 1.5, 2.5, 0.6);
    CHECK_STR(scratch, "0|2|2|1");
    snprintf(scratch, sizeof scratch, "%.2f|%.1f|%.1f", 1.005, 0.05, 0.25);
    CHECK_STR(scratch, "1.00|0.1|0.2");
    snprintf(scratch, sizeof scratch, "%.0f|%.2f|%.6f", 123.456, 123.456, 123.456);
    CHECK_STR(scratch, "123|123.46|123.456000");
    snprintf(scratch, sizeof scratch, "%f|%.3f|%.2f", 3.14159, 9.9996, -1.5);
    CHECK_STR(scratch, "3.141590|10.000|-1.50");
    snprintf(scratch, sizeof scratch, "%8.2f|%-8.2f|%08.2f|%+.1f", 3.14159, 3.14159, -3.14159, 2.0);
    CHECK_STR(scratch, "    3.14|3.14    |-0003.14|+2.0");
    snprintf(scratch, sizeof scratch, "%.0f|%f|%.3f", 0.0, 0.0, 1234567890123.0);
    CHECK_STR(scratch, "0|0.000000|1234567890123.000");
    snprintf(scratch, sizeof scratch, "%.22f", 0.5);
    CHECK_STR(scratch, "0.5000000000000000000000");
}

static void test_snprintf_limits(void)
{
    char small[8];
    memset(small, 'x', sizeof small);
    CHECK(snprintf(small, sizeof small, "%s", "0123456789") == 10);
    CHECK_STR(small, "0123456");
    CHECK(snprintf(NULL, 0, "abc%d", 12) == 5);
    memset(small, 'x', sizeof small);
    CHECK(snprintf(small, 1, "abc") == 3);
    CHECK(small[0] == '\0' && small[1] == 'x');
    CHECK(snprintf(small, sizeof small, "%d", 123) == 3);
}

static void test_sscanf(void)
{
    int value = -1;
    unsigned unsigned_value = 0;
    int second = -1;

    CHECK(sscanf("1f", "%x", &unsigned_value) == 1 && unsigned_value == 0x1f);
    CHECK(sscanf("0x1F", "%x", &unsigned_value) == 1 && unsigned_value == 0x1f);
    CHECK(sscanf("0x10", "%i", &value) == 1 && value == 16);
    CHECK(sscanf("010", "%i", &value) == 1 && value == 8);
    CHECK(sscanf("54", "%i", &value) == 1 && value == 54);
    CHECK(sscanf("  0x1f", " 0x%x", &unsigned_value) == 1 && unsigned_value == 0x1f);
    CHECK(sscanf("0X2A", " 0X%x", &unsigned_value) == 1 && unsigned_value == 0x2a);
    CHECK(sscanf("077", " 0%o", &unsigned_value) == 1 && unsigned_value == 63);
    CHECK(sscanf("  -12", " %d", &value) == 1 && value == -12);
    CHECK(sscanf("7 8", "%d %d", &value, &second) == 2 && value == 7 && second == 8);
    CHECK(sscanf("7 x", "%d %d", &value, &second) == 1);
    CHECK(sscanf("zz", "%d", &value) == 0);
    CHECK(sscanf("", "%d", &value) == EOF);
    CHECK(sscanf("abc", "abd%d", &value) == 0);
    CHECK(sscanf("4000000000", "%u", &unsigned_value) == 1 && unsigned_value == 4000000000u);
}

static void test_conversions(void)
{
    char *end = NULL;

    CHECK(strtol("  -123abc", &end, 10) == -123 && strcmp(end, "abc") == 0);
    CHECK(strtol("0x1f", NULL, 0) == 31);
    CHECK(strtol("0x1f", NULL, 16) == 31);
    CHECK(strtol("077", NULL, 0) == 63);
    CHECK(strtol("+19", NULL, 10) == 19);
    CHECK(strtol("12", &end, 2) == 1 && strcmp(end, "2") == 0);
    CHECK(strtol("zz", &end, 10) == 0 && end != NULL && strcmp(end, "zz") == 0);
    errno = 0;
    CHECK(strtol("99999999999999999999", NULL, 10) == LONG_MAX && errno == ERANGE);
    errno = 0;
    CHECK(strtol("-99999999999999999999", NULL, 10) == LONG_MIN && errno == ERANGE);
    CHECK(strtol("-9223372036854775808", NULL, 10) == LONG_MIN);
    CHECK(atoi("  31x") == 31);

    CHECK(strtod("3.5", NULL) == 3.5);
    CHECK(strtod("-0.25xyz", &end) == -0.25 && strcmp(end, "xyz") == 0);
    CHECK(strtod("1e3", NULL) == 1000.0);
    CHECK(strtod("2.5E-1", NULL) == 0.25);
    CHECK(strtod(".5", NULL) == 0.5);
    CHECK(strtod("abc", &end) == 0.0 && strcmp(end, "abc") == 0);
    CHECK(strtod("1e", &end) == 1.0 && strcmp(end, "e") == 0);
    CHECK(strtod("100", NULL) == 100.0);
}

static void test_strings(void)
{
    const char *text = "hello world";
    char fill[8];

    CHECK(strlen("") == 0 && strlen(text) == 11);
    CHECK(strcmp("abc", "abc") == 0 && strcmp("abc", "abd") < 0 && strcmp("b", "a") > 0);
    CHECK(strcmp("ab", "abc") < 0 && strcmp("\xff", "a") > 0);
    CHECK(strncmp("abcdef", "abcxyz", 3) == 0 && strncmp("abcdef", "abcxyz", 4) < 0);
    CHECK(strncmp("a", "b", 0) == 0);
    CHECK(strcasecmp("DoOm", "dOOm") == 0 && strcasecmp("a", "B") < 0 && strcasecmp("b", "A") > 0);
    CHECK(strncasecmp("HELLOx", "helloy", 5) == 0 && strncasecmp("HELLOx", "helloy", 6) < 0);
    CHECK(strchr(text, 'o') == text + 4 && strchr(text, 'z') == NULL && strchr(text, '\0') == text + 11);
    CHECK(strrchr(text, 'o') == text + 7 && strrchr(text, 'z') == NULL && strrchr(text, '\0') == text + 11);
    CHECK(strstr(text, "o w") == text + 4 && strstr(text, "") == text && strstr(text, "xyz") == NULL);
    CHECK(strstr(text, "world") == text + 6 && strstr(text, "worlds") == NULL);
    CHECK(memchr(text, 'w', 11) == text + 6 && memchr(text, 'w', 6) == NULL);

    memset(fill, 'x', sizeof fill);
    CHECK(strncpy(fill, "ab", 5) == fill);
    CHECK(fill[0] == 'a' && fill[1] == 'b' && fill[2] == 0 && fill[4] == 0 && fill[5] == 'x');

    char *copy = strdup(text);
    CHECK(copy != NULL && copy != text && strcmp(copy, text) == 0);
    copy[0] = 'J';
    CHECK(text[0] == 'h');
    free(copy);
}

static int block_holds_pattern(const unsigned char *block, size_t size, unsigned char seed)
{
    for (size_t index = 0; index < size; index++) {
        if (block[index] != (unsigned char)(seed + index)) {
            return 0;
        }
    }

    return 1;
}

static void test_memory(void)
{
    static const size_t sizes[] = {1, 15, 16, 17, 4096, 4097, 100000, 8 * 1024 * 1024};

    for (size_t slot = 0; slot < sizeof sizes / sizeof sizes[0]; slot++) {
        size_t size = sizes[slot];
        unsigned char *block = malloc(size);
        CHECK(block != NULL && (uintptr_t)block % MALLOC_ALIGN_BYTES == 0);
        if (block == NULL) {
            continue;
        }

        for (size_t index = 0; index < size; index++) {
            block[index] = (unsigned char)(0x5a + index);
        }
        CHECK(block_holds_pattern(block, size, 0x5a));
        free(block);
    }

    unsigned char *grown = malloc(100);
    for (size_t index = 0; index < 100; index++) {
        grown[index] = (unsigned char)(3 + index);
    }
    grown = realloc(grown, 10000);
    CHECK(grown != NULL && block_holds_pattern(grown, 100, 3));
    grown = realloc(grown, 40);
    CHECK(grown != NULL && block_holds_pattern(grown, 40, 3));
    free(grown);

    unsigned char *fresh = realloc(NULL, 24);
    CHECK(fresh != NULL && (uintptr_t)fresh % MALLOC_ALIGN_BYTES == 0);
    CHECK(realloc(fresh, 0) == NULL);

    unsigned char *dirty = malloc(300);
    memset(dirty, 0xaa, 300);
    free(dirty);
    unsigned char *zeroed = calloc(30, 10);
    int all_zero = zeroed != NULL;
    for (size_t index = 0; all_zero && index < 300; index++) {
        all_zero = zeroed[index] == 0;
    }
    CHECK(all_zero);
    free(zeroed);
    CHECK(calloc((size_t)-1, 2) == NULL);
    free(NULL);
}

static void test_file_reads(void)
{
    char buffer[32];

    FILE *hello = fopen("/HELLO.TXT", "rb");
    CHECK(hello != NULL);
    if (hello == NULL) {
        return;
    }

    size_t size = fread(buffer, 1, sizeof buffer, hello);
    CHECK(size > 0 && size < sizeof buffer && memcmp(buffer, "hello", 5) == 0);
    CHECK(ftell(hello) == (long)size && fread(buffer, 1, 1, hello) == 0);
    CHECK(fseek(hello, 1, SEEK_SET) == 0 && fread(buffer, 1, 1, hello) == 1 && buffer[0] == 'e');
    CHECK(fseek(hello, 1, SEEK_CUR) == 0 && ftell(hello) == 3);
    CHECK(fseek(hello, -2, SEEK_END) == 0 && ftell(hello) == (long)size - 2);
    CHECK(fseek(hello, -1, SEEK_SET) == -1 && ftell(hello) == (long)size - 2);
    CHECK(fread(buffer, 2, 1, hello) == 1);
    CHECK(fclose(hello) == 0);

    for (int cycle = 0; cycle < FILE_CYCLE_COUNT; cycle++) {
        FILE *again = fopen("/HELLO.TXT", "r");
        CHECK(again != NULL);
        if (again != NULL) {
            fclose(again);
        }
    }

    errno = 0;
    CHECK(fopen("/NOPE.TXT", "rb") == NULL && errno == ENOENT);
    errno = 0;
    CHECK(fopen("/HELLO.TXT", "w") == NULL && errno == EROFS);
    errno = 0;
    CHECK(fopen("/HELLO.TXT", "r+") == NULL && errno == EROFS);
    errno = 0;
    CHECK(fopen("/HELLO.TXT", "ab") == NULL && errno == EROFS);
    errno = 0;
    CHECK(remove("/HELLO.TXT") == -1 && errno == EROFS);
    errno = 0;
    CHECK(rename("/HELLO.TXT", "/OTHER.TXT") == -1 && errno == EROFS);
    errno = 0;
    CHECK(mkdir("/NEWDIR", 0755) == -1 && errno == EROFS);
    CHECK(system(NULL) == -1);
}

/* the same reads on BIG.BIN, preloaded or not depending on the limit set before */
static void check_pattern_file(void)
{
    static const size_t offsets[] = {0, 1, 250, 251, 4095, 4096, 50000, 99990};
    unsigned char bytes[10];

    FILE *file = fopen("/BIG.BIN", "rb");
    CHECK(file != NULL);
    if (file == NULL) {
        return;
    }

    for (size_t slot = 0; slot < sizeof offsets / sizeof offsets[0]; slot++) {
        size_t offset = offsets[slot];
        int matches = fseek(file, (long)offset, SEEK_SET) == 0 && fread(bytes, 1, sizeof bytes, file) == sizeof bytes;
        for (size_t index = 0; matches && index < sizeof bytes; index++) {
            matches = bytes[index] == pattern_byte(offset + index);
        }
        CHECK(matches);
    }

    CHECK(fseek(file, -5, SEEK_END) == 0 && fread(bytes, 1, sizeof bytes, file) == 5);
    CHECK(bytes[0] == pattern_byte(PATTERN_FILE_BYTES - 5));
    CHECK(fseek(file, 0, SEEK_END) == 0 && ftell(file) == PATTERN_FILE_BYTES && fread(bytes, 1, 1, file) == 0);
    CHECK(fseek(file, 99998, SEEK_SET) == 0 && fseek(file, 1, SEEK_CUR) == 0 && ftell(file) == 99999);

    static unsigned char block[PATTERN_PERIOD * 40];
    CHECK(fseek(file, 7, SEEK_SET) == 0 && fread(block, 1, sizeof block, file) == sizeof block);
    int whole_block_matches = 1;
    for (size_t index = 0; index < sizeof block; index++) {
        whole_block_matches &= block[index] == pattern_byte(7 + index);
    }
    CHECK(whole_block_matches);
    CHECK(fclose(file) == 0);
}

static void test_pattern_file(void)
{
    /* the default limit preloads it */
    check_pattern_file();

    /* a small limit sends it through the kernel on every read */
    bigc_set_preload_max_bytes(SMALL_PRELOAD_LIMIT_BYTES);
    check_pattern_file();
    bigc_set_preload_max_bytes(DEFAULT_PRELOAD_LIMIT_BYTES);
}

void libctest_main(void)
{
    test_integer_formats();
    test_text_formats();
    test_float_formats();
    test_snprintf_limits();
    test_sscanf();
    test_conversions();
    test_strings();
    test_memory();
    test_file_reads();
    test_pattern_file();
}

void libctest_print(void)
{
    char line[LONG_LINE_BYTES + 1];
    libctest_fill_long_line(line);

    printf("%s|%d\n", line, 54);
    fprintf(stdout, "%s\n", "second");
    puts("end");
    putchar('!');
    putchar('\n');
}

void libctest_exit(void)
{
    exit(EXIT_CHECK_CODE);
}
