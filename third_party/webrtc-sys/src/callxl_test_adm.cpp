/*
 * CallXL: WAV-driven test AudioDeviceModule. MIT, CallXL authors.
 *
 * - Capture: the whole 48 kHz mono PCM16 WAV is preloaded at construction and
 *   looped, 10 ms (480 samples) per tick, into AudioTransport::RecordedDataIsAvailable.
 * - Playout: NeedMorePlayData output is appended to a buffer pre-sized at
 *   construction (record_max_secs); it is written to `record_wav` on Terminate()
 *   or on an explicit flush.
 * - One audio thread ticks on absolute CLOCK_MONOTONIC deadlines. After Init()
 *   the audio thread performs no allocation, no logging and takes no lock: all
 *   shared state is atomics. Control calls come from the worker thread.
 */
#include <time.h>

#include <atomic>
#include <chrono>
#include <cstdio>
#include <cstring>
#include <string>
#include <thread>
#include <vector>

#include "api/audio/audio_device.h"
#include "api/audio/audio_device_defines.h"
#include "api/make_ref_counted.h"
#include "livekit/callxl.h"
#include "livekit/peer_connection_factory.h"
#include "rtc_base/logging.h"

namespace callxl {

namespace {
constexpr int kRate = 48000;
constexpr size_t kFrame = kRate / 100;  // 10 ms

bool LoadWav(const std::string& path, std::vector<int16_t>& out) {
  FILE* f = std::fopen(path.c_str(), "rb");
  if (!f) return false;
  std::vector<uint8_t> bytes;
  uint8_t buf[65536];
  size_t n;
  while ((n = std::fread(buf, 1, sizeof(buf), f)) > 0) bytes.insert(bytes.end(), buf, buf + n);
  std::fclose(f);
  if (bytes.size() < 12 || std::memcmp(bytes.data(), "RIFF", 4) != 0 ||
      std::memcmp(bytes.data() + 8, "WAVE", 4) != 0)
    return false;
  size_t pos = 12;
  uint16_t fmt = 0, ch = 0, bits = 0;
  uint32_t rate = 0;
  while (pos + 8 <= bytes.size()) {
    uint32_t len;
    std::memcpy(&len, bytes.data() + pos + 4, 4);
    const uint8_t* body = bytes.data() + pos + 8;
    if (pos + 8 + len > bytes.size()) len = static_cast<uint32_t>(bytes.size() - pos - 8);
    if (std::memcmp(bytes.data() + pos, "fmt ", 4) == 0 && len >= 16) {
      std::memcpy(&fmt, body, 2);
      std::memcpy(&ch, body + 2, 2);
      std::memcpy(&rate, body + 4, 4);
      std::memcpy(&bits, body + 14, 2);
    } else if (std::memcmp(bytes.data() + pos, "data", 4) == 0) {
      if (fmt != 1 || ch != 1 || bits != 16 || rate != kRate) return false;
      out.resize(len / 2);
      std::memcpy(out.data(), body, out.size() * 2);
      return !out.empty();
    }
    pos += 8 + len + (len & 1);
  }
  return false;
}

bool WriteWav(const std::string& path, const int16_t* data, size_t samples) {
  FILE* f = std::fopen(path.c_str(), "wb");
  if (!f) return false;
  const uint32_t data_len = static_cast<uint32_t>(samples * 2);
  const uint32_t riff_len = 36 + data_len;
  const uint16_t fmt = 1, ch = 1, bits = 16, align = 2;
  const uint32_t rate = kRate, byte_rate = kRate * 2, fmt_len = 16;
  std::fwrite("RIFF", 1, 4, f);
  std::fwrite(&riff_len, 4, 1, f);
  std::fwrite("WAVEfmt ", 1, 8, f);
  std::fwrite(&fmt_len, 4, 1, f);
  std::fwrite(&fmt, 2, 1, f);
  std::fwrite(&ch, 2, 1, f);
  std::fwrite(&rate, 4, 1, f);
  std::fwrite(&byte_rate, 4, 1, f);
  std::fwrite(&align, 2, 1, f);
  std::fwrite(&bits, 2, 1, f);
  std::fwrite("data", 1, 4, f);
  std::fwrite(&data_len, 4, 1, f);
  std::fwrite(data, 2, samples, f);
  return std::fclose(f) == 0;
}

int64_t NowUs() {
  timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return static_cast<int64_t>(ts.tv_sec) * 1000000 + ts.tv_nsec / 1000;
}
}  // namespace

class TestAudioDevice : public webrtc::AudioDeviceModule {
 public:
  TestAudioDevice(const std::string& play_wav,
                  const std::string& record_wav,
                  uint32_t record_max_secs)
      : record_path_(record_wav) {
    if (!play_wav.empty() && !LoadWav(play_wav, play_)) {
      RTC_LOG(LS_ERROR) << "CallXL test ADM: cannot load " << play_wav
                        << " (need 48 kHz mono PCM16); capturing silence";
    }
    if (play_.empty()) play_.assign(kFrame, 0);
    // Pad to a whole number of frames so the loop never splits a frame.
    while (play_.size() % kFrame) play_.push_back(0);
    if (!record_path_.empty()) record_.resize(size_t{record_max_secs} * kRate);
    scratch_.resize(kFrame * 2);

  }

  ~TestAudioDevice() override { Terminate(); }

  int32_t ActiveAudioLayer(AudioLayer* layer) const override {
    *layer = AudioLayer::kDummyAudio;
    return 0;
  }
  int32_t RegisterAudioCallback(webrtc::AudioTransport* t) override {
    transport_.store(t, std::memory_order_release);
    return 0;
  }
  int32_t Init() override {
    if (initialized_) return 0;
    initialized_ = true;
    running_.store(true);
    thread_ = std::thread([this] { Run(); });
    return 0;
  }
  int32_t Terminate() override {
    if (!initialized_) return 0;
    running_.store(false);
    if (thread_.joinable()) thread_.join();
    initialized_ = false;
    Flush();
    return 0;
  }
  bool Initialized() const override { return initialized_; }

  int16_t PlayoutDevices() override { return 1; }
  int16_t RecordingDevices() override { return 1; }
  int32_t PlayoutDeviceName(uint16_t, char name[webrtc::kAdmMaxDeviceNameSize],
                            char guid[webrtc::kAdmMaxGuidSize]) override {
    std::snprintf(name, webrtc::kAdmMaxDeviceNameSize, "CallXL test playout (WAV)");
    std::snprintf(guid, webrtc::kAdmMaxGuidSize, "callxl-test-out");
    return 0;
  }
  int32_t RecordingDeviceName(uint16_t, char name[webrtc::kAdmMaxDeviceNameSize],
                              char guid[webrtc::kAdmMaxGuidSize]) override {
    std::snprintf(name, webrtc::kAdmMaxDeviceNameSize, "CallXL test capture (WAV)");
    std::snprintf(guid, webrtc::kAdmMaxGuidSize, "callxl-test-in");
    return 0;
  }
  int32_t SetPlayoutDevice(uint16_t) override { return 0; }
  int32_t SetPlayoutDevice(WindowsDeviceType) override { return 0; }
  int32_t SetRecordingDevice(uint16_t) override { return 0; }
  int32_t SetRecordingDevice(WindowsDeviceType) override { return 0; }

  int32_t PlayoutIsAvailable(bool* a) override { *a = true; return 0; }
  int32_t InitPlayout() override { play_init_ = true; return 0; }
  bool PlayoutIsInitialized() const override { return play_init_; }
  int32_t RecordingIsAvailable(bool* a) override { *a = true; return 0; }
  int32_t InitRecording() override { rec_init_ = true; return 0; }
  bool RecordingIsInitialized() const override { return rec_init_; }

  int32_t StartPlayout() override { playing_.store(true); return 0; }
  int32_t StopPlayout() override { playing_.store(false); return 0; }
  bool Playing() const override { return playing_.load(); }
  int32_t StartRecording() override { recording_.store(true); return 0; }
  int32_t StopRecording() override { recording_.store(false); return 0; }
  bool Recording() const override { return recording_.load(); }

  int32_t InitSpeaker() override { return 0; }
  bool SpeakerIsInitialized() const override { return true; }
  int32_t InitMicrophone() override { return 0; }
  bool MicrophoneIsInitialized() const override { return true; }
  int32_t SpeakerVolumeIsAvailable(bool* a) override { *a = false; return 0; }
  int32_t SetSpeakerVolume(uint32_t) override { return -1; }
  int32_t SpeakerVolume(uint32_t*) const override { return -1; }
  int32_t MaxSpeakerVolume(uint32_t*) const override { return -1; }
  int32_t MinSpeakerVolume(uint32_t*) const override { return -1; }
  int32_t MicrophoneVolumeIsAvailable(bool* a) override { *a = false; return 0; }
  int32_t SetMicrophoneVolume(uint32_t) override { return -1; }
  int32_t MicrophoneVolume(uint32_t*) const override { return -1; }
  int32_t MaxMicrophoneVolume(uint32_t*) const override { return -1; }
  int32_t MinMicrophoneVolume(uint32_t*) const override { return -1; }
  int32_t SpeakerMuteIsAvailable(bool* a) override { *a = false; return 0; }
  int32_t SetSpeakerMute(bool) override { return -1; }
  int32_t SpeakerMute(bool*) const override { return -1; }
  int32_t MicrophoneMuteIsAvailable(bool* a) override { *a = false; return 0; }
  int32_t SetMicrophoneMute(bool) override { return -1; }
  int32_t MicrophoneMute(bool*) const override { return -1; }
  int32_t StereoPlayoutIsAvailable(bool* a) const override { *a = false; return 0; }
  int32_t SetStereoPlayout(bool enable) override { return enable ? -1 : 0; }
  int32_t StereoPlayout(bool* e) const override { *e = false; return 0; }
  int32_t StereoRecordingIsAvailable(bool* a) const override { *a = false; return 0; }
  int32_t SetStereoRecording(bool enable) override { return enable ? -1 : 0; }
  int32_t StereoRecording(bool* e) const override { *e = false; return 0; }
  int32_t PlayoutDelay(uint16_t* d) const override { *d = 0; return 0; }
  bool BuiltInAECIsAvailable() const override { return false; }
  bool BuiltInAGCIsAvailable() const override { return false; }
  bool BuiltInNSIsAvailable() const override { return false; }
  int32_t EnableBuiltInAEC(bool) override { return -1; }
  int32_t EnableBuiltInAGC(bool) override { return -1; }
  int32_t EnableBuiltInNS(bool) override { return -1; }
#if defined(WEBRTC_IOS)
  int GetPlayoutAudioParameters(webrtc::AudioParameters*) const override { return -1; }
  int GetRecordAudioParameters(webrtc::AudioParameters*) const override { return -1; }
#endif
  int32_t SetObserver(webrtc::AudioDeviceObserver*) override { return 0; }

  bool Flush() {
    if (record_path_.empty()) return false;
    const size_t n = recorded_.load(std::memory_order_acquire);
    return WriteWav(record_path_, record_.data(), n);
  }

  void Stats(livekit_ffi::CallXlTestAdmStats& s) const {
    s.active = running_.load();
    s.recorded_frames = recorded_.load() / kFrame;
    s.played_frames = captured_frames_.load();
    s.max_tick_late_us = max_late_us_.load();
    s.late_ticks = late_ticks_.load();
    s.record_capacity_frames = record_.size() / kFrame;
  }

 private:
  void Run() {
    int64_t next = NowUs();
    size_t play_pos = 0;
    while (running_.load(std::memory_order_relaxed)) {
      next += 10000;
#if defined(__APPLE__)
      // No clock_nanosleep on Apple: sleep for what is left until the same absolute
      // deadline (NowUs' clock), so the 10 ms cadence still does not drift.
      const int64_t wait = next - NowUs();
      if (wait > 0) std::this_thread::sleep_for(std::chrono::microseconds(wait));
#else
      timespec ts{static_cast<time_t>(next / 1000000),
                  static_cast<long>((next % 1000000) * 1000)};
      clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &ts, nullptr);
#endif
      const int64_t late = NowUs() - next;
      if (late > max_late_us_.load(std::memory_order_relaxed))
        max_late_us_.store(late, std::memory_order_relaxed);
      if (late > 5000) late_ticks_.fetch_add(1, std::memory_order_relaxed);
      if (late > 100000) next = NowUs();  // resync after a stall, never burst

      webrtc::AudioTransport* t = transport_.load(std::memory_order_acquire);
      if (!t) continue;
      if (recording_.load(std::memory_order_relaxed)) {
        uint32_t new_level = 0;
        t->RecordedDataIsAvailable(play_.data() + play_pos, kFrame, 2, 1, kRate, 0, 0,
                                   0, false, new_level);
        play_pos += kFrame;
        if (play_pos >= play_.size()) play_pos = 0;
        captured_frames_.fetch_add(1, std::memory_order_relaxed);
      }
      if (playing_.load(std::memory_order_relaxed)) {
        size_t out = 0;
        int64_t elapsed = 0, ntp = 0;
        t->NeedMorePlayData(kFrame, 2, 1, kRate, scratch_.data(), out, &elapsed, &ntp);
        const size_t at = recorded_.load(std::memory_order_relaxed);
        if (out > kFrame) out = kFrame;
        if (at + kFrame <= record_.size()) {
          std::memcpy(record_.data() + at, scratch_.data(), out * 2);
          if (out < kFrame) std::memset(record_.data() + at + out, 0, (kFrame - out) * 2);
          recorded_.store(at + kFrame, std::memory_order_release);
        }
      }
    }
  }

  std::vector<int16_t> play_;
  std::vector<int16_t> record_;
  std::vector<int16_t> scratch_;
  std::string record_path_;
  std::atomic<webrtc::AudioTransport*> transport_{nullptr};
  std::atomic<bool> running_{false};
  std::atomic<bool> playing_{false};
  std::atomic<bool> recording_{false};
  std::atomic<size_t> recorded_{0};
  std::atomic<uint64_t> captured_frames_{0};
  std::atomic<int64_t> max_late_us_{0};
  std::atomic<uint64_t> late_ticks_{0};
  bool initialized_ = false;
  bool play_init_ = false;
  bool rec_init_ = false;
  std::thread thread_;
};

webrtc::scoped_refptr<webrtc::AudioDeviceModule> CreateTestAudioDevice(
    const webrtc::Environment&,
    const std::string& play_wav,
    const std::string& record_wav,
    uint32_t record_max_secs) {
  return webrtc::scoped_refptr<webrtc::AudioDeviceModule>(
      webrtc::make_ref_counted<TestAudioDevice>(play_wav, record_wav, record_max_secs));
}

bool TestAudioDeviceFlush(webrtc::AudioDeviceModule* adm) {
  return adm && static_cast<TestAudioDevice*>(adm)->Flush();
}

void TestAudioDeviceStats(webrtc::AudioDeviceModule* adm,
                          livekit_ffi::CallXlTestAdmStats& out) {
  if (adm) static_cast<TestAudioDevice*>(adm)->Stats(out);
}

}  // namespace callxl
