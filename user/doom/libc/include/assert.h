#pragma once

void bigc_assert_fail(const char *expression, const char *file, int line)
    __attribute__((noreturn));

#ifdef NDEBUG
#define assert(expression) ((void)0)
#else
#define assert(expression) \
    ((expression) ? (void)0 : bigc_assert_fail(#expression, __FILE__, __LINE__))
#endif
