/*
 * Minimal C ABI around Inuitive's C++ InuDev SDK.
 *
 * The SDK (libInuStreams) only exposes a C++ interface (std::shared_ptr,
 * std::function callbacks, ...). This shim hides that behind a flat C API so
 * the Rust side can talk to it without hand written C++ name mangling.
 *
 * On Windows the functions must be exported explicitly, which is what
 * INU_SHIM_API takes care of.
 */
#ifndef INU_SHIM_H
#define INU_SHIM_H

#include <stddef.h>
#include <stdint.h>

#ifdef _WIN32
#define INU_SHIM_API __declspec(dllexport)
#else
#define INU_SHIM_API __attribute__((visibility("default")))
#endif

#ifdef __cplusplus
extern "C" {
#endif

typedef struct InuContext InuContext;

/* Output pixel format requested from the CImageStream. */
enum InuOutputFormat {
    INU_FORMAT_BGRA = 0,
    INU_FORMAT_BGR  = 1,
    INU_FORMAT_RGBA = 2
};

/* Streams to open, combined as a bitmask in InuOpenOptions.streams. The same
 * values identify the stream in the frame callback. */
#define INU_STREAM_RGB   0x1u
#define INU_STREAM_DEPTH 0x2u

typedef struct InuOpenOptions {
    /* InuService name to connect to, NULL/empty means "auto detect". */
    const char* service_name;
    /* Requested frame rate, 0 means the sensor default. */
    uint32_t fps;
    /* RGB channel id to stream from, < 0 means "pick the RGB channel". */
    int32_t channel_id;
    /* One of InuOutputFormat (RGB stream only). */
    int32_t output_format;
    /* Bitmask of INU_STREAM_* to open. 0 means INU_STREAM_RGB. */
    uint32_t streams;
    /* 1 to have the chip register depth to the RGB camera (ActivateRegisteredDepth),
     * so depth and rgb share one camera frame with no manual alignment. */
    int32_t registered_depth;
} InuOpenOptions;

/*
 * Called for every frame delivered by the SDK, from an internal SDK thread.
 * `stream` is the INU_STREAM_* bit of the stream that produced the frame.
 * `data` points at `height` rows of `stride` bytes and is only valid for the
 * duration of the call (copy it if you keep it around).
 */
typedef void (*InuFrameCallback)(int32_t stream,
                                 const unsigned char* data,
                                 int32_t width,
                                 int32_t height,
                                 int32_t stride,
                                 int32_t format,
                                 uint64_t timestamp,
                                 void* user);

/* Create the sensor and initialize it. Returns NULL on failure. */
INU_SHIM_API InuContext* inu_open(const InuOpenOptions* options);

/* Register the frame callback. Must be called before inu_start(). */
INU_SHIM_API int32_t inu_set_frame_callback(InuContext* ctx,
                                            InuFrameCallback callback,
                                            void* user);

/* Create + start the RGB stream. Returns 0 on success, -1 on failure. */
INU_SHIM_API int32_t inu_start(InuContext* ctx);

/* Stop streaming and release the sensor. Safe to call more than once. */
INU_SHIM_API void inu_stop(InuContext* ctx);

/* Stop (if needed) and free the context. */
INU_SHIM_API void inu_close(InuContext* ctx);

/* Human readable description of the last failed call (thread local). */
INU_SHIM_API const char* inu_last_error(void);

/* Channel id the context is streaming from, or UINT32_MAX if unknown. */
INU_SHIM_API uint32_t inu_channel_id(const InuContext* ctx);

/* Number of channels discovered by inu_open(). 0 when unavailable. */
INU_SHIM_API uint32_t inu_channel_count(const InuContext* ctx);

/* Channel id by discovery index, or UINT32_MAX when out of range. */
INU_SHIM_API uint32_t inu_channel_at(const InuContext* ctx, uint32_t index);

/* InuDev::EChannelType of a channel (1 = RGB, 3 = IR stereo, 4 = depth, ...). */
INU_SHIM_API int32_t inu_channel_type(const InuContext* ctx, uint32_t index);

/* Number of sensors discovered by inu_open(). 0 when unavailable. */
INU_SHIM_API uint32_t inu_sensor_count(const InuContext* ctx);

/* Sensor id by discovery index. */
INU_SHIM_API uint32_t inu_sensor_at(const InuContext* ctx, uint32_t index);

/* InuDev::ESensorModel of a sensor (132 = CGS_132 color, 130 = AR_130, ...). */
INU_SHIM_API int32_t inu_sensor_model(const InuContext* ctx, uint32_t index);

/* InuDev::ESensorRole of a sensor (0 = left, 1 = right, 2 = mono/color). */
INU_SHIM_API int32_t inu_sensor_role(const InuContext* ctx, uint32_t index);

/* Bitmask of INU_STREAM_* that inu_start() actually got running. */
INU_SHIM_API uint32_t inu_active_streams(const InuContext* ctx);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* INU_SHIM_H */
