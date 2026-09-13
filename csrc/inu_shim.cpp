/*
 * Minimal C ABI around Inuitive's C++ InuDev SDK. See inu_shim.h.
 *
 * Built by build.rs into libinu_shim.so / inu_shim.dll and loaded at runtime
 * with dlopen / LoadLibrary, so the main binary never links InuDev directly.
 */
#include "inu_shim.h"

#include <cstdio>
#include <map>
#include <memory>
#include <string>
#include <vector>

#include "InuSensor.h"
#include "HwInformation.h"
#include "ImageFrame.h"
#include "ImageStream.h"
#include "DepthStream.h"

namespace {

thread_local std::string g_lastError;

void setLastError(const std::string& message)
{
    g_lastError = message;
}

void setErrorFromCode(const char* what, const InuDev::CInuError& error)
{
    char code[32];
    std::snprintf(code, sizeof(code), "0x%08x", static_cast<unsigned int>(static_cast<int>(error)));
    g_lastError = std::string(what) + " failed: " + code + " " + static_cast<std::string>(error);
}

} // namespace

struct InuChannelEntry {
    uint32_t id = InuDev::DEFAULT_CHANNEL_ID;
    int32_t type = 0;
};

struct InuSensorEntry {
    uint32_t id = 0;
    int32_t model = 0;
    int32_t role = 0;
};

struct InuContext {
    std::shared_ptr<InuDev::CInuSensor> sensor;
    std::shared_ptr<InuDev::CImageStream> imageStream;
    std::shared_ptr<InuDev::CDepthStream> depthStream;
    uint32_t rgbChannel = InuDev::DEFAULT_CHANNEL_ID;
    uint32_t depthChannel = InuDev::DEFAULT_CHANNEL_ID;
    std::vector<InuChannelEntry> channels;
    std::vector<InuSensorEntry> sensors;
    InuFrameCallback callback = nullptr;
    void* user = nullptr;
    uint32_t fps = 0;
    int32_t outputFormat = INU_FORMAT_BGRA;
    uint32_t streams = INU_STREAM_RGB;
    int32_t registeredDepth = 0;
    uint32_t activeStreams = 0;
    bool sensorStarted = false;
    bool loggedDepthInfo = false;
};

/// Forward one SDK frame to the Rust callback. Shared by every stream, which
/// all deliver `CImageFrame`.
void deliverFrame(InuContext* ctx, int32_t stream, const InuDev::CImageFrame& frame)
{
    if (!ctx->callback || !frame.Valid)
        return;

    const int32_t width = static_cast<int32_t>(frame.Width());
    const int32_t height = static_cast<int32_t>(frame.Height());
    const int32_t bpp = static_cast<int32_t>(frame.BytesPerPixel());
    if (width <= 0 || height <= 0 || bpp <= 0)
        return;

    ctx->callback(stream,
                  frame.GetData(),
                  width,
                  height,
                  width * bpp,
                  static_cast<int32_t>(frame.Format()),
                  frame.Timestamp,
                  ctx->user);
}

extern "C" INU_SHIM_API InuContext* inu_open(const InuOpenOptions* options)
{
    g_lastError.clear();

    std::string serviceName;
    uint32_t fps = 0;
    int32_t requestedChannel = -1;
    uint32_t streams = INU_STREAM_RGB;
    int32_t registeredDepth = 0;
    if (options) {
        if (options->service_name)
            serviceName = options->service_name;
        fps = options->fps;
        requestedChannel = options->channel_id;
        if (options->streams != 0)
            streams = options->streams;
        registeredDepth = options->registered_depth;
    }

    auto sensor = InuDev::CInuSensor::Create(serviceName);
    if (!sensor) {
        setLastError("CInuSensor::Create returned null "
                     "(is InuService running and the NU4000 connected?)");
        return nullptr;
    }

    InuDev::CHwInformation hwInfo;
    InuDev::CInuError error = sensor->Init(hwInfo);
    if (error != InuDev::EErrorCode::eOK) {
        setErrorFromCode("CInuSensor::Init", error);
        return nullptr;
    }

    auto* ctx = new InuContext();
    ctx->sensor = sensor;
    ctx->fps = fps;
    ctx->streams = streams;
    ctx->registeredDepth = registeredDepth;

    uint32_t rgbChannel = InuDev::DEFAULT_CHANNEL_ID;
    uint32_t depthChannel = InuDev::DEFAULT_CHANNEL_ID;
    for (const auto& entry : hwInfo.GetChannels()) {
        const uint32_t id = entry.first;
        const InuDev::CHwChannel& channel = entry.second;
        InuChannelEntry info;
        info.id = id;
        info.type = static_cast<int32_t>(channel.ChannelType);
        ctx->channels.push_back(info);
        std::fprintf(stderr, "[inu-r132] channel %u: type=%d\n", id, info.type);
        if (rgbChannel == InuDev::DEFAULT_CHANNEL_ID
            && channel.ChannelType == InuDev::eGeneralCameraChannel) {
            rgbChannel = id;
        }
        if (depthChannel == InuDev::DEFAULT_CHANNEL_ID
            && channel.ChannelType == InuDev::eDepthChannel) {
            depthChannel = id;
        }
    }

    for (const auto& entry : hwInfo.GetSensors()) {
        const InuDev::CSensorParams& params = entry.second;
        InuSensorEntry info;
        info.id = entry.first;
        info.model = static_cast<int32_t>(params.Model);
        info.role = static_cast<int32_t>(params.Role);
        ctx->sensors.push_back(info);
        std::fprintf(stderr, "[inu-r132] sensor %u: model=%d role=%d\n",
                     info.id, info.model, info.role);
    }

    if (requestedChannel >= 0)
        rgbChannel = static_cast<uint32_t>(requestedChannel);
    if ((streams & INU_STREAM_RGB) && rgbChannel == InuDev::DEFAULT_CHANNEL_ID)
        std::fprintf(stderr, "[inu-r132] warning: no RGB channel found\n");
    if ((streams & INU_STREAM_DEPTH) && depthChannel == InuDev::DEFAULT_CHANNEL_ID)
        std::fprintf(stderr, "[inu-r132] warning: no depth channel found\n");
    std::fprintf(stderr, "[inu-r132] requested streams=0x%x, rgb channel=%d, depth channel=%d\n",
                 streams,
                 rgbChannel == InuDev::DEFAULT_CHANNEL_ID ? -1 : static_cast<int>(rgbChannel),
                 depthChannel == InuDev::DEFAULT_CHANNEL_ID ? -1 : static_cast<int>(depthChannel));

    ctx->rgbChannel = rgbChannel;
    ctx->depthChannel = depthChannel;
    return ctx;
}

extern "C" INU_SHIM_API int32_t inu_set_frame_callback(InuContext* ctx,
                                                       InuFrameCallback callback,
                                                       void* user)
{
    if (!ctx) {
        setLastError("inu_set_frame_callback: null context");
        return -1;
    }
    ctx->callback = callback;
    ctx->user = user;
    return 0;
}

extern "C" INU_SHIM_API int32_t inu_start(InuContext* ctx)
{
    if (!ctx || !ctx->sensor) {
        setLastError("inu_start: invalid context");
        return -1;
    }
    if (!ctx->callback) {
        setLastError("inu_start: no frame callback registered");
        return -1;
    }
    g_lastError.clear();

    if (!ctx->sensorStarted) {
        InuDev::CInuError error = InuDev::EErrorCode::eOK;
        const bool wantRgb = (ctx->streams & INU_STREAM_RGB)
            && ctx->rgbChannel != InuDev::DEFAULT_CHANNEL_ID;
        const bool wantDepth = (ctx->streams & INU_STREAM_DEPTH)
            && ctx->depthChannel != InuDev::DEFAULT_CHANNEL_ID;

        if (ctx->registeredDepth && wantDepth) {
            // Ask the chip to register depth to the RGB camera: it uses the
            // factory calibration (intrinsics, distortion, extrinsics), which is
            // exactly what a plain image offset cannot do. This mirrors the
            // official depth_reg_example, which starts the depth channel with
            // ActivateRegisteredDepth.
            std::map<uint32_t, InuDev::CChannelSize> channelSizes;
            std::map<uint32_t, InuDev::CChannelControlParams> channelParams;
            if (wantRgb) {
                InuDev::CChannelControlParams params;
                params.SensorRes = InuDev::eDefaultResolution;
                if (ctx->fps > 0)
                    params.FPS = ctx->fps;
                channelParams[ctx->rgbChannel] = params;
            }
            InuDev::CChannelControlParams params;
            params.SensorRes = InuDev::eDefaultResolution;
            if (ctx->fps > 0)
                params.FPS = ctx->fps;
            params.ActivateRegisteredDepth = true;
            // Like the official example, leave RegisteredDepthChannelID at its
            // default: the SDK picks the associated colour channel itself.
            channelParams[ctx->depthChannel] = params;
            error = ctx->sensor->Start(channelSizes, channelParams);
            std::fprintf(stderr, "[inu-r132] registered depth requested on channel %u\n",
                         ctx->depthChannel);
        } else if ((ctx->streams == INU_STREAM_RGB) && wantRgb) {
            std::map<uint32_t, InuDev::CChannelSize> channelSizes;
            std::map<uint32_t, InuDev::CChannelControlParams> channelParams;
            InuDev::CChannelControlParams params;
            params.SensorRes = InuDev::eDefaultResolution;
            if (ctx->fps > 0)
                params.FPS = ctx->fps;
            channelParams[ctx->rgbChannel] = params;
            error = ctx->sensor->Start(channelSizes, channelParams);
        } else {
            // The official depth sample otherwise starts the sensor without
            // channel parameters; the SDK brings up whatever the graph needs.
            error = ctx->sensor->Start();
        }
        if (error != InuDev::EErrorCode::eOK) {
            setErrorFromCode("CInuSensor::Start", error);
            return -1;
        }
        ctx->sensorStarted = true;
    }

    InuContext* c = ctx;
    ctx->activeStreams = 0;

    if (ctx->streams & INU_STREAM_RGB) {
        ctx->imageStream = ctx->sensor->CreateImageStream(ctx->rgbChannel);
        if (!ctx->imageStream) {
            std::fprintf(stderr, "[inu-r132] CreateImageStream returned null\n");
        } else {
            InuDev::CImageStream::EOutputFormat format = InuDev::CImageStream::eBGRA;
            if (ctx->outputFormat == INU_FORMAT_BGR)
                format = InuDev::CImageStream::eBGR;
            else if (ctx->outputFormat == INU_FORMAT_RGBA)
                format = InuDev::CImageStream::eRGBA;

            InuDev::CInuError error = ctx->imageStream->Init(format, InuDev::CImageStream::eNone);
            if (error != InuDev::EErrorCode::eOK) {
                setErrorFromCode("CImageStream::Init", error);
                std::fprintf(stderr, "[inu-r132] RGB stream init failed: %s\n", g_lastError.c_str());
            } else if ((error = ctx->imageStream->Start()) != InuDev::EErrorCode::eOK) {
                setErrorFromCode("CImageStream::Start", error);
                std::fprintf(stderr, "[inu-r132] RGB stream start failed: %s\n", g_lastError.c_str());
            } else if ((error = ctx->imageStream->Register(
                            [c](std::shared_ptr<InuDev::CImageStream> /*stream*/,
                                std::shared_ptr<const InuDev::CImageFrame> frame,
                                InuDev::CInuError frameError) {
                                if (frameError != InuDev::EErrorCode::eOK || !frame)
                                    return;
                                deliverFrame(c, INU_STREAM_RGB, *frame);
                            })) != InuDev::EErrorCode::eOK) {
                setErrorFromCode("CImageStream::Register", error);
                std::fprintf(stderr, "[inu-r132] RGB stream register failed: %s\n",
                             g_lastError.c_str());
            } else {
                ctx->activeStreams |= INU_STREAM_RGB;
            }
        }
    }

    if (ctx->streams & INU_STREAM_DEPTH) {
        ctx->depthStream = ctx->sensor->CreateDepthStream(ctx->depthChannel);
        if (!ctx->depthStream) {
            std::fprintf(stderr, "[inu-r132] CreateDepthStream returned null "
                                 "(does this device have a depth channel?)\n");
        } else {
            InuDev::CInuError error = ctx->depthStream->Init(InuDev::CDepthStream::eDepth);
            if (error != InuDev::EErrorCode::eOK) {
                setErrorFromCode("CDepthStream::Init", error);
                std::fprintf(stderr, "[inu-r132] depth stream init failed: %s\n",
                             g_lastError.c_str());
            } else if ((error = ctx->depthStream->Start()) != InuDev::EErrorCode::eOK) {
                setErrorFromCode("CDepthStream::Start", error);
                std::fprintf(stderr, "[inu-r132] depth stream start failed: %s\n",
                             g_lastError.c_str());
            } else if ((error = ctx->depthStream->Register(
                            [c](std::shared_ptr<InuDev::CDepthStream> /*stream*/,
                                std::shared_ptr<const InuDev::CImageFrame> frame,
                                InuDev::CInuError frameError) {
                                if (frameError != InuDev::EErrorCode::eOK || !frame)
                                    return;
                                if (!c->loggedDepthInfo) {
                                    c->loggedDepthInfo = true;
                                    std::fprintf(stderr,
                                                 "[inu-r132] depth frame %ux%u format=%d bpp=%u "
                                                 "registration=%d undistortion=%d\n",
                                                 frame->Width(), frame->Height(),
                                                 static_cast<int32_t>(frame->Format()),
                                                 frame->BytesPerPixel(),
                                                 static_cast<int32_t>(frame->GetDepthRegistrationType()),
                                                 static_cast<int32_t>(frame->GetUndistortionType()));
                                }
                                deliverFrame(c, INU_STREAM_DEPTH, *frame);
                            })) != InuDev::EErrorCode::eOK) {
                setErrorFromCode("CDepthStream::Register", error);
                std::fprintf(stderr, "[inu-r132] depth stream register failed: %s\n",
                             g_lastError.c_str());
            } else {
                ctx->activeStreams |= INU_STREAM_DEPTH;
            }
        }
    }

    if (ctx->activeStreams == 0) {
        setLastError("none of the requested streams could be started");
        return -1;
    }
    std::fprintf(stderr, "[inu-r132] active streams=0x%x\n", ctx->activeStreams);
    return 0;
}

extern "C" INU_SHIM_API void inu_stop(InuContext* ctx)
{
    if (!ctx)
        return;

    if (ctx->imageStream) {
        ctx->imageStream->Register(nullptr);
        ctx->imageStream->Stop();
        ctx->imageStream->Terminate();
        ctx->imageStream.reset();
    }

    if (ctx->depthStream) {
        ctx->depthStream->Register(nullptr);
        ctx->depthStream->Stop();
        ctx->depthStream->Terminate();
        ctx->depthStream.reset();
    }

    if (ctx->sensor && ctx->sensorStarted) {
        ctx->sensor->Stop();
        ctx->sensor->Terminate();
    }
    ctx->sensorStarted = false;
    ctx->activeStreams = 0;
}

extern "C" INU_SHIM_API void inu_close(InuContext* ctx)
{
    if (!ctx)
        return;
    inu_stop(ctx);
    delete ctx;
}

extern "C" INU_SHIM_API const char* inu_last_error(void)
{
    return g_lastError.c_str();
}

extern "C" INU_SHIM_API uint32_t inu_channel_id(const InuContext* ctx)
{
    if (!ctx)
        return InuDev::DEFAULT_CHANNEL_ID;
    if (ctx->rgbChannel != InuDev::DEFAULT_CHANNEL_ID)
        return ctx->rgbChannel;
    return ctx->depthChannel;
}

extern "C" INU_SHIM_API uint32_t inu_channel_count(const InuContext* ctx)
{
    return ctx ? static_cast<uint32_t>(ctx->channels.size()) : 0;
}

extern "C" INU_SHIM_API uint32_t inu_channel_at(const InuContext* ctx, uint32_t index)
{
    if (!ctx || index >= ctx->channels.size())
        return InuDev::DEFAULT_CHANNEL_ID;
    return ctx->channels[index].id;
}

extern "C" INU_SHIM_API int32_t inu_channel_type(const InuContext* ctx, uint32_t index)
{
    if (!ctx || index >= ctx->channels.size())
        return 0;
    return ctx->channels[index].type;
}

extern "C" INU_SHIM_API uint32_t inu_sensor_count(const InuContext* ctx)
{
    return ctx ? static_cast<uint32_t>(ctx->sensors.size()) : 0;
}

extern "C" INU_SHIM_API uint32_t inu_sensor_at(const InuContext* ctx, uint32_t index)
{
    if (!ctx || index >= ctx->sensors.size())
        return 0;
    return ctx->sensors[index].id;
}

extern "C" INU_SHIM_API int32_t inu_sensor_model(const InuContext* ctx, uint32_t index)
{
    if (!ctx || index >= ctx->sensors.size())
        return 0;
    return ctx->sensors[index].model;
}

extern "C" INU_SHIM_API int32_t inu_sensor_role(const InuContext* ctx, uint32_t index)
{
    if (!ctx || index >= ctx->sensors.size())
        return 0;
    return ctx->sensors[index].role;
}

extern "C" INU_SHIM_API uint32_t inu_active_streams(const InuContext* ctx)
{
    return ctx ? ctx->activeStreams : 0;
}
