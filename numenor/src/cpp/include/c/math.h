#pragma once

#ifdef __cplusplus
extern "C" {
#endif

double round(double x);
float roundf(float x);
float log2f(float x);

#define FP_NAN 0
#define FP_INFINITE 1
#define FP_ZERO 2
#define FP_SUBNORMAL 3
#define FP_NORMAL 4

#define INFINITY (__builtin_inff())
#define NAN (__builtin_nanf(""))
#define HUGE_VALF (__builtin_huge_valf())
#define HUGE_VAL (__builtin_huge_val())
#define HUGE_VALL (__builtin_huge_vall())

#define isfinite(x) __builtin_isfinite(x)
#define isinf(x) __builtin_isinf(x)
#define isnan(x) __builtin_isnan(x)

#define M_PI 3.14159265358979323846
#define M_PI_2 1.57079632679489661923
#define M_SQRT2 1.41421356237309504880

double sqrt(double x);
float sqrtf(float x);
double log(double x);
float logf(float x);
double exp(double x);
float expf(float x);
double pow(double x, double y);
float powf(float x, float y);
double sin(double x);
float sinf(float x);
double cos(double x);
float cosf(float x);
double tan(double x);
float tanf(float x);
double tanh(double x);
float tanhf(float x);
float expm1f(float x);
float erff(float x);
double asin(double x);
float asinf(float x);
double acos(double x);
float acosf(float x);
double atan(double x);
float atanf(float x);
double atan2(double y, double x);
float atan2f(float y, float x);

double ceil(double x);
float ceilf(float x);
double floor(double x);
float floorf(float x);
double fmod(double x, double y);
float fmodf(float x, float y);
double fmin(double x, double y);
float fminf(float x, float y);
double fmax(double x, double y);
float fmaxf(float x, float y);
double fabs(double x);
float fabsf(float x);
double trunc(double x);
float truncf(float x);

long lroundf(float x);

#ifdef __cplusplus
}
#endif
