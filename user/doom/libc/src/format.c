#include <assert.h>
#include <stdarg.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define FORMAT_BUFFER_BYTES 256
#define INTEGER_BUFFER_BYTES 32
#define FLOAT_DIGIT_BUFFER_BYTES 64
#define DEFAULT_FLOAT_PRECISION 6
#define MAX_FLOAT_PRECISION 40
#define EXACT_FRACTION_DIGITS 19
#define DECIMAL_BASE 10
#define OCTAL_BASE 8
#define HEX_BASE 16
#define UINT64_RANGE_AS_DOUBLE 18446744073709551616.0

size_t bigc_write(FILE *stream, const char *bytes, size_t length);

/* 
a string sink writes into buffer and keeps counting past its capacity
a stream sink collects into chunk and flushes through bigc_write when the chunk is full 
*/
struct sink {
    char *buffer;
    size_t capacity;
    size_t total;
    FILE *stream;
    char chunk[FORMAT_BUFFER_BYTES];
    size_t chunk_length;
};

enum flag {
    FLAG_LEFT = 1 << 0,
    FLAG_ZERO = 1 << 1,
    FLAG_PLUS = 1 << 2,
    FLAG_SPACE = 1 << 3,
    FLAG_ALTERNATE = 1 << 4,
};

enum length_modifier {
    LENGTH_NONE,
    LENGTH_CHAR,
    LENGTH_SHORT,
    LENGTH_LONG,
    LENGTH_LONG_LONG,
    LENGTH_SIZE,
};

struct spec {
    int flags;
    int width;
    int precision;
    enum length_modifier length;
};

static void sink_flush(struct sink *sink)
{
    if (sink->chunk_length > 0) {
        bigc_write(sink->stream, sink->chunk, sink->chunk_length);
        sink->chunk_length = 0;
    }
}

static void sink_put(struct sink *sink, const char *bytes, size_t count)
{
    if (sink->stream != NULL) {
        for (size_t index = 0; index < count; index++) {
            if (sink->chunk_length == FORMAT_BUFFER_BYTES) {
                sink_flush(sink);
            }
            sink->chunk[sink->chunk_length++] = bytes[index];
        }
    } else {
        for (size_t index = 0; index < count; index++) {
            /* one byte stays free for the terminating NUL */
            if (sink->total + index + 1 < sink->capacity) {
                sink->buffer[sink->total + index] = bytes[index];
            }
        }
    }

    sink->total += count;
}

static void sink_repeat(struct sink *sink, char byte, int count)
{
    for (int index = 0; index < count; index++) {
        sink_put(sink, &byte, 1);
    }
}

static void sink_finish(struct sink *sink)
{
    if (sink->stream != NULL) {
        sink_flush(sink);
    } else if (sink->capacity > 0) {
        size_t end = sink->total < sink->capacity - 1 ? sink->total : sink->capacity - 1;
        sink->buffer[end] = '\0';
    }
}

static void emit_number(struct sink *sink, const struct spec *spec, const char *prefix,
                        size_t prefix_length, const char *digits, size_t digit_length,
                        int precision_zeros)
{
    int content = (int)(prefix_length + digit_length) + precision_zeros;
    int padding = spec->width > content ? spec->width - content : 0;
    int zero_pad = (spec->flags & FLAG_ZERO) && !(spec->flags & FLAG_LEFT);

    if (!(spec->flags & FLAG_LEFT) && !zero_pad) {
        sink_repeat(sink, ' ', padding);
    }
    sink_put(sink, prefix, prefix_length);
    if (zero_pad) {
        sink_repeat(sink, '0', padding);
    }
    sink_repeat(sink, '0', precision_zeros);
    sink_put(sink, digits, digit_length);
    if (spec->flags & FLAG_LEFT) {
        sink_repeat(sink, ' ', padding);
    }
}

static void format_integer(struct sink *sink, struct spec *spec, unsigned long long magnitude,
                           int negative, unsigned base, int uppercase, int is_signed)
{
    const char *alphabet = uppercase ? "0123456789ABCDEF" : "0123456789abcdef";
    char digits[INTEGER_BUFFER_BYTES];
    size_t digit_length = 0;
    char prefix[2];
    size_t prefix_length = 0;

    if (magnitude != 0 || spec->precision != 0) {
        do {
            digits[INTEGER_BUFFER_BYTES - 1 - digit_length] = alphabet[magnitude % base];
            digit_length++;
            magnitude /= base;
        } while (magnitude != 0);
    }
    const char *digit_start = digits + INTEGER_BUFFER_BYTES - digit_length;

    int precision_zeros = spec->precision > (int)digit_length ? spec->precision - (int)digit_length : 0;
    if (spec->precision >= 0) {
        spec->flags &= ~FLAG_ZERO;
    }

    if (is_signed) {
        if (negative) {
            prefix[prefix_length++] = '-';
        } else if (spec->flags & FLAG_PLUS) {
            prefix[prefix_length++] = '+';
        } else if (spec->flags & FLAG_SPACE) {
            prefix[prefix_length++] = ' ';
        }
    } else if ((spec->flags & FLAG_ALTERNATE) && base == HEX_BASE && digit_length > 0 &&
               (digit_start[0] != '0' || digit_length > 1)) {
        prefix[prefix_length++] = '0';
        prefix[prefix_length++] = uppercase ? 'X' : 'x';
    } else if ((spec->flags & FLAG_ALTERNATE) && base == OCTAL_BASE && precision_zeros == 0 &&
               (digit_length == 0 || digit_start[0] != '0')) {
        precision_zeros = 1;
    }

    emit_number(sink, spec, prefix, prefix_length, digit_start, digit_length, precision_zeros);
}

static void format_string(struct sink *sink, const struct spec *spec, const char *text)
{
    if (text == NULL) {
        text = "(null)";
    }

    size_t length = 0;
    while (text[length] != '\0' && (spec->precision < 0 || (int)length < spec->precision)) {
        length++;
    }

    int padding = spec->width > (int)length ? spec->width - (int)length : 0;
    if (!(spec->flags & FLAG_LEFT)) {
        sink_repeat(sink, ' ', padding);
    }
    sink_put(sink, text, length);
    if (spec->flags & FLAG_LEFT) {
        sink_repeat(sink, ' ', padding);
    }
}

static void format_float(struct sink *sink, struct spec *spec, double value)
{
    char text[FLOAT_DIGIT_BUFFER_BYTES];
    int negative = value < 0 || (value == 0 && 1 / value < 0);
    double magnitude = negative ? -value : value;
    int precision = spec->precision < 0 ? DEFAULT_FLOAT_PRECISION : spec->precision;
    assert(precision <= MAX_FLOAT_PRECISION);

    if (magnitude != magnitude || magnitude >= UINT64_RANGE_AS_DOUBLE) {
        spec->flags &= ~FLAG_ZERO;
        const char *special = magnitude != magnitude ? "nan" : "inf";
        emit_number(sink, spec, negative ? "-" : "", negative ? 1 : 0, special, 3, 0);

        return;
    }

    unsigned long long integer_part = (unsigned long long)magnitude;
    double fraction = magnitude - (double)integer_part;
    unsigned long long fixed = (unsigned long long)(fraction * UINT64_RANGE_AS_DOUBLE);

    char fraction_digits[EXACT_FRACTION_DIGITS];
    int exact_digits = precision < EXACT_FRACTION_DIGITS ? precision : EXACT_FRACTION_DIGITS;
    for (int index = 0; index < exact_digits; index++) {
        unsigned __int128 scaled = (unsigned __int128)fixed * DECIMAL_BASE;
        fraction_digits[index] = (char)('0' + (int)(scaled >> 64));
        fixed = (unsigned long long)scaled;
    }

    if (precision <= EXACT_FRACTION_DIGITS) {
        const unsigned long long half = 1ULL << 63;
        int last_is_odd = exact_digits > 0 ? (fraction_digits[exact_digits - 1] - '0') & 1
                                           : (int)(integer_part & 1);
        int round_up = fixed > half || (fixed == half && last_is_odd);
        for (int index = exact_digits - 1; round_up && index >= 0; index--) {
            if (fraction_digits[index] == '9') {
                fraction_digits[index] = '0';
            } else {
                fraction_digits[index]++;
                round_up = 0;
            }
        }
        if (round_up) {
            integer_part++;
        }
    }

    size_t length = 0;
    char integer_digits[INTEGER_BUFFER_BYTES];
    size_t integer_length = 0;
    do {
        integer_digits[integer_length++] = (char)('0' + integer_part % DECIMAL_BASE);
        integer_part /= DECIMAL_BASE;
    } while (integer_part != 0);
    while (integer_length > 0) {
        text[length++] = integer_digits[--integer_length];
    }
    if (precision > 0) {
        text[length++] = '.';
        for (int index = 0; index < exact_digits; index++) {
            text[length++] = fraction_digits[index];
        }
    }

    char prefix[1];
    size_t prefix_length = 0;
    if (negative) {
        prefix[prefix_length++] = '-';
    } else if (spec->flags & FLAG_PLUS) {
        prefix[prefix_length++] = '+';
    } else if (spec->flags & FLAG_SPACE) {
        prefix[prefix_length++] = ' ';
    }

    int extra_zeros = precision - exact_digits;
    int content = (int)(prefix_length + length) + extra_zeros;
    int padding = spec->width > content ? spec->width - content : 0;
    int zero_pad = (spec->flags & FLAG_ZERO) && !(spec->flags & FLAG_LEFT);
    if (!(spec->flags & FLAG_LEFT) && !zero_pad) {
        sink_repeat(sink, ' ', padding);
    }
    sink_put(sink, prefix, prefix_length);
    if (zero_pad) {
        sink_repeat(sink, '0', padding);
    }
    sink_put(sink, text, length);
    sink_repeat(sink, '0', extra_zeros);
    if (spec->flags & FLAG_LEFT) {
        sink_repeat(sink, ' ', padding);
    }
}

static unsigned long long fetch_unsigned(va_list *arguments, enum length_modifier length)
{
    switch (length) {
    case LENGTH_CHAR:
        return (unsigned char)va_arg(*arguments, unsigned int);
    case LENGTH_SHORT:
        return (unsigned short)va_arg(*arguments, unsigned int);
    case LENGTH_LONG:
        return va_arg(*arguments, unsigned long);
    case LENGTH_LONG_LONG:
        return va_arg(*arguments, unsigned long long);
    case LENGTH_SIZE:
        return va_arg(*arguments, size_t);
    default:
        return va_arg(*arguments, unsigned int);
    }
}

static long long fetch_signed(va_list *arguments, enum length_modifier length)
{
    switch (length) {
    case LENGTH_CHAR:
        return (signed char)va_arg(*arguments, int);
    case LENGTH_SHORT:
        return (short)va_arg(*arguments, int);
    case LENGTH_LONG:
        return va_arg(*arguments, long);
    case LENGTH_LONG_LONG:
        return va_arg(*arguments, long long);
    case LENGTH_SIZE:
        return (long long)va_arg(*arguments, size_t);
    default:
        return va_arg(*arguments, int);
    }
}

static int parse_number(const char **cursor)
{
    int value = 0;
    while (**cursor >= '0' && **cursor <= '9') {
        value = value * DECIMAL_BASE + (**cursor - '0');
        (*cursor)++;
    }

    return value;
}

static void bigc_format(struct sink *sink, const char *format, va_list arguments)
{
    va_list copy;
    va_copy(copy, arguments);

    while (*format != '\0') {
        if (*format != '%') {
            const char *run = format;
            while (*format != '\0' && *format != '%') {
                format++;
            }
            sink_put(sink, run, (size_t)(format - run));
            continue;
        }
        format++;

        struct spec spec = {0, 0, -1, LENGTH_NONE};
        for (;; format++) {
            if (*format == '-') {
                spec.flags |= FLAG_LEFT;
            } else if (*format == '0') {
                spec.flags |= FLAG_ZERO;
            } else if (*format == '+') {
                spec.flags |= FLAG_PLUS;
            } else if (*format == ' ') {
                spec.flags |= FLAG_SPACE;
            } else if (*format == '#') {
                spec.flags |= FLAG_ALTERNATE;
            } else {
                break;
            }
        }

        if (*format == '*') {
            spec.width = va_arg(copy, int);
            if (spec.width < 0) {
                spec.flags |= FLAG_LEFT;
                spec.width = -spec.width;
            }
            format++;
        } else {
            spec.width = parse_number(&format);
        }

        if (*format == '.') {
            format++;
            if (*format == '*') {
                spec.precision = va_arg(copy, int);
                format++;
            } else {
                spec.precision = parse_number(&format);
            }
        }

        if (*format == 'h') {
            format++;
            spec.length = LENGTH_SHORT;
            if (*format == 'h') {
                format++;
                spec.length = LENGTH_CHAR;
            }
        } else if (*format == 'l') {
            format++;
            spec.length = LENGTH_LONG;
            if (*format == 'l') {
                format++;
                spec.length = LENGTH_LONG_LONG;
            }
        } else if (*format == 'z') {
            format++;
            spec.length = LENGTH_SIZE;
        }

        char conversion = *format++;
        switch (conversion) {
        case 'd':
        case 'i': {
            long long value = fetch_signed(&copy, spec.length);
            unsigned long long magnitude = value < 0 ? 0ULL - (unsigned long long)value
                                                     : (unsigned long long)value;
            format_integer(sink, &spec, magnitude, value < 0, DECIMAL_BASE, 0, 1);
            break;
        }
        case 'u':
            format_integer(sink, &spec, fetch_unsigned(&copy, spec.length), 0, DECIMAL_BASE, 0, 0);
            break;
        case 'o':
            format_integer(sink, &spec, fetch_unsigned(&copy, spec.length), 0, OCTAL_BASE, 0, 0);
            break;
        case 'x':
            format_integer(sink, &spec, fetch_unsigned(&copy, spec.length), 0, HEX_BASE, 0, 0);
            break;
        case 'X':
            format_integer(sink, &spec, fetch_unsigned(&copy, spec.length), 0, HEX_BASE, 1, 0);
            break;
        case 'p':
            spec.flags |= FLAG_ALTERNATE;
            format_integer(sink, &spec, (uintptr_t)va_arg(copy, void *), 0, HEX_BASE, 0, 0);
            break;
        case 'c': {
            char character = (char)va_arg(copy, int);
            char text[1] = {character};
            int padding = spec.width > 1 ? spec.width - 1 : 0;
            if (!(spec.flags & FLAG_LEFT)) {
                sink_repeat(sink, ' ', padding);
            }
            sink_put(sink, text, 1);
            if (spec.flags & FLAG_LEFT) {
                sink_repeat(sink, ' ', padding);
            }
            break;
        }
        case 's':
            format_string(sink, &spec, va_arg(copy, const char *));
            break;
        case 'f':
        case 'F':
            format_float(sink, &spec, va_arg(copy, double));
            break;
        case '%':
            sink_put(sink, "%", 1);
            break;
        default:
            assert(!"unsupported printf conversion");
        }
    }

    va_end(copy);
}

int vsnprintf(char *buffer, size_t capacity, const char *format, va_list arguments)
{
    struct sink sink = {buffer, capacity, 0, NULL, {0}, 0};
    bigc_format(&sink, format, arguments);
    sink_finish(&sink);

    return (int)sink.total;
}

int snprintf(char *buffer, size_t capacity, const char *format, ...)
{
    va_list arguments;
    va_start(arguments, format);
    int written = vsnprintf(buffer, capacity, format, arguments);
    va_end(arguments);

    return written;
}

int vfprintf(FILE *stream, const char *format, va_list arguments)
{
    struct sink sink = {NULL, 0, 0, stream, {0}, 0};
    bigc_format(&sink, format, arguments);
    sink_finish(&sink);

    return (int)sink.total;
}

int fprintf(FILE *stream, const char *format, ...)
{
    va_list arguments;
    va_start(arguments, format);
    int written = vfprintf(stream, format, arguments);
    va_end(arguments);

    return written;
}

int printf(const char *format, ...)
{
    va_list arguments;
    va_start(arguments, format);
    int written = vfprintf(stdout, format, arguments);
    va_end(arguments);

    return written;
}

static const char *skip_whitespace(const char *text)
{
    while (*text == ' ' || (*text >= '\t' && *text <= '\r')) {
        text++;
    }

    return text;
}

int sscanf(const char *input, const char *format, ...)
{
    va_list arguments;
    va_start(arguments, format);
    int converted = 0;
    int failed = 0;

    while (*format != '\0' && !failed) {
        if (*format == ' ' || (*format >= '\t' && *format <= '\r')) {
            input = skip_whitespace(input);
            format++;
        } else if (*format != '%') {
            if (*input != *format) {
                failed = 1;
            } else {
                input++;
                format++;
            }
        } else {
            format++;
            int base;
            switch (*format) {
            case 'd':
                base = DECIMAL_BASE;
                break;
            case 'i':
                base = 0;
                break;
            case 'x':
                base = HEX_BASE;
                break;
            case 'o':
                base = OCTAL_BASE;
                break;
            case 'u':
                base = DECIMAL_BASE;
                break;
            default:
                assert(!"unsupported sscanf conversion");
                base = DECIMAL_BASE;
            }
            format++;

            input = skip_whitespace(input);
            if (*input == '\0' && converted == 0) {
                va_end(arguments);

                return EOF;
            }

            char *end;
            long value = strtol(input, &end, base);
            if (end == input) {
                failed = 1;
            } else {
                *va_arg(arguments, int *) = (int)value;
                converted++;
                input = end;
            }
        }
    }

    va_end(arguments);

    return converted;
}
