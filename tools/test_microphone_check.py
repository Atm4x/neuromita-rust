import io
import json
import subprocess
import unittest
import wave

from serve_microphone_check import decode_report, prepare_audio


def make_wav(pcm, rate=16000):
    stream = io.BytesIO()
    with wave.open(stream, 'wb') as wav:
        wav.setparams((1, 2, rate, 0, 'NONE', 'not compressed'))
        wav.writeframes(pcm)
    return stream.getvalue()


class MicrophoneCheckTests(unittest.TestCase):
    def test_arbitrary_recording_lengths_preserve_pcm_and_bound_padding(self):
        for frames in (1, 255, 256, 511, 512, 513, 16001, 480000):
            with self.subTest(frames=frames):
                original = b'\x12\x34' * frames
                pcm, count, padding = prepare_audio(make_wav(original))
                self.assertEqual(count, frames)
                self.assertEqual(pcm[:frames * 2], original)
                self.assertEqual(pcm[frames * 2:], b'\0\0' * padding)
                self.assertLess(padding, 512)
                self.assertEqual((frames + padding) % 512, 0)

    def test_invalid_audio_is_rejected(self):
        for data in (make_wav(b''), make_wav(b'\0\0', rate=48000),
                     make_wav(b'\0\0' * 480001), make_wav(b'\0\0' * 512)[:-1]):
            with self.subTest(size=len(data)), self.assertRaises(ValueError):
                prepare_audio(data)

    def test_failure_keeps_rust_summary_and_stderr(self):
        result = subprocess.CompletedProcess([], 1, json.dumps(dict(
            provider='direct', error='truncated PCM16 frame', sample_count=256
        )).encode(), b'probe failed; see JSON summary')
        report = decode_report(result, 'direct', 257, 255)
        self.assertEqual(report['error'], 'truncated PCM16 frame')
        self.assertEqual(report['sample_count'], 256)
        self.assertEqual(report['exit_code'], 1)
        self.assertIn('probe failed', report['stderr'])

    def test_failure_without_json_still_has_diagnostic(self):
        result = subprocess.CompletedProcess([], 1, b'', b'model missing')
        self.assertEqual(decode_report(result, 'silero', 1, 511)['error'], 'model missing')

    def test_timeout_or_partial_analysis_cannot_look_successful(self):
        for finish, count in (('duration', 0), ('duration', 512), ('eof', 256)):
            result = subprocess.CompletedProcess([], 0, json.dumps(dict(
                provider='silero', error=None, finish=finish, sample_count=count
            )).encode(), b'')
            self.assertIsNotNone(decode_report(result, 'silero', 257, 255)['error'])
        result = subprocess.CompletedProcess([], 0, json.dumps(dict(
            provider='silero', error=None, finish='eof', sample_count=512
        )).encode(), b'')
        self.assertIsNone(decode_report(result, 'silero', 257, 255)['error'])


if __name__ == '__main__':
    unittest.main()
