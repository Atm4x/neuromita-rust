"""Loopback-only microphone UI and optional local native VAD analysis."""

import argparse
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import wave
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = Path(__file__).resolve().parent.parent
MAX_BYTES = 44 + 16000 * 2 * 30


def prepare_audio(data):
    with wave.open(io.BytesIO(data)) as wav:
        if (wav.getnchannels(), wav.getsampwidth(), wav.getframerate(), wav.getcomptype()) != (1, 2, 16000, 'NONE'):
            raise ValueError('Требуется mono PCM16 WAV 16 кГц')
        frames = wav.getnframes()
        if not 0 < frames <= 480000:
            raise ValueError('Некорректный размер WAV')
        pcm = wav.readframes(frames)
        if len(pcm) != frames * 2:
            raise ValueError('Некорректный размер WAV')
    padding = (-frames) % 512
    return pcm + b'\0\0' * padding, frames, padding


def decode_report(result, provider, frames, padding):
    stderr = result.stderr.decode('utf-8', errors='replace')[-3000:]
    try:
        report = json.loads(result.stdout)
        if not isinstance(report, dict):
            raise ValueError('Expected JSON object')
    except (ValueError, UnicodeDecodeError):
        report = dict(provider=provider, error=stderr or 'Rust не вернул JSON-отчёт')
    if result.returncode and not report.get('error'):
        report['error'] = stderr or f'Rust exit {result.returncode}'
    if not report.get('error') and (report.get('finish') != 'eof'
                                   or report.get('sample_count') != frames + padding):
        report['error'] = 'Анализ записи не завершён: получены не все PCM-кадры. Повторите проверку после освобождения ресурсов.'
    report.update(original_sample_count=frames, padding_samples=padding,
                  exit_code=result.returncode, stderr=stderr)
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--port', type=int, default=8768)
    parser.add_argument('--probe', type=Path, required=True)
    parser.add_argument('--ffmpeg', type=Path, required=True)
    parser.add_argument('--model', type=Path, required=True)
    parser.add_argument('--ort', type=Path, required=True)
    args = parser.parse_args()
    for path in (args.probe, args.ffmpeg, args.model, args.ort):
        if not path.is_absolute() or not path.is_file():
            parser.error(f'Explicit existing absolute file required: {path}')
    origin = f'http://127.0.0.1:{args.port}'
    lock = threading.Lock()
    cache = ROOT / '.cache'
    cache.mkdir(exist_ok=True)

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def send(self, code, body, content_type='application/json'):
            data = body.encode() if isinstance(body, str) else json.dumps(body).encode()
            self.send_response(code)
            self.send_header('Content-Type', content_type)
            self.send_header('Content-Length', str(len(data)))
            self.send_header('Cache-Control', 'no-store')
            self.send_header('X-Content-Type-Options', 'nosniff')
            self.end_headers()
            self.wfile.write(data)

        def allowed(self):
            return self.headers.get('Host') == f'127.0.0.1:{args.port}'

        def do_GET(self):
            if not self.allowed():
                self.send(403, {'error': 'Loopback host required'})
            elif self.path == '/':
                self.send(200, (ROOT / 'tools/microphone-check.html').read_text(encoding='utf-8'), 'text/html; charset=utf-8')
            elif self.path == '/config':
                self.send(200, {'local': True})
            else:
                self.send(404, {'error': 'Not found'})

        def do_POST(self):
            self.connection.settimeout(10)
            if not self.allowed() or self.headers.get('Origin') != origin:
                self.send(403, {'error': 'Local page required'})
                return
            if self.path != '/analyze':
                self.send(404, {'error': 'Not found'})
                return
            if not lock.acquire(blocking=False):
                self.send(409, {'error': 'Другая проверка ещё выполняется'})
                return
            try:
                length = int(self.headers.get('Content-Length', '0'))
                if not 44 < length <= MAX_BYTES:
                    raise ValueError('Нужна запись WAV длиной до 30 секунд')
                data = self.rfile.read(length)
                if len(data) != length:
                    raise ValueError('Неполная запись')
                pcm, frames, padding = prepare_audio(data)
                analysis_frames = frames + padding
                reports = []
                with tempfile.TemporaryDirectory(prefix='mic_', dir=cache) as tmp:
                    directory = Path(tmp)
                    audio = directory / 'input.wav'
                    with wave.open(str(audio), 'wb') as wav:
                        wav.setparams((1, 2, 16000, 0, 'NONE', 'not compressed'))
                        wav.writeframes(pcm)
                    relative = audio.relative_to(ROOT).as_posix()
                    env = dict(os.environ, NEUROMITA_SILERO_MODEL=str(args.model), NEUROMITA_ORT_DLL=str(args.ort))
                    for provider in ('direct', 'silero'):
                        config = dict(ffmpeg_binary=str(args.ffmpeg), backend='lavfi',
                                      device=f'amovie={relative},atrim=end_sample={analysis_frames}',
                                      provider=provider, threshold=0.5, duration_secs=40,
                                      mode='auto', ptt_events=[], stop_at_ms=None, output_dir=None,
                                      ptt_policy=None, segment=dict(pre_roll_samples=3072,
                                      start_voice_samples=768, end_silence_samples=7168,
                                      min_segment_samples=2560, max_segment_samples=160000))
                        config_path = directory / f'{provider}.json'
                        config_path.write_text(json.dumps(config), encoding='utf-8')
                        try:
                            result = subprocess.run([str(args.probe), str(config_path)], cwd=ROOT,
                                                    env=env, capture_output=True, timeout=50,
                                                    creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
                            reports.append(decode_report(result, provider, frames, padding))
                        except subprocess.TimeoutExpired:
                            reports.append(dict(provider=provider, error='Превышено время локального анализа'))
                self.send(200, reports)
            except (ValueError, wave.Error, EOFError) as exc:
                self.send(400, {'error': str(exc)})
            except Exception as exc:
                self.send(500, {'error': f'Локальный анализ завершился ошибкой: {exc}'})
            finally:
                lock.release()

    server = ThreadingHTTPServer(('127.0.0.1', args.port), Handler)
    server.daemon_threads = True
    print(origin, flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == '__main__':
    main()
