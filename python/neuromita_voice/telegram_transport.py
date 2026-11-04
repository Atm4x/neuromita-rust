"""One-request Telegram TTS transport for an existing authorized Telethon session.

Protocol: one JSON request on stdin, base64 audio on stdout; errors only on stderr.
The caller is responsible for converting MP3/OGG into WAV and managing worker lifetime.
"""

import asyncio
import base64
import json
import os
import re
import sys
import time


async def main():
    from telethon import TelegramClient
    from telethon.tl.types import MessageMediaDocument, DocumentAttributeAudio

    request = json.loads(sys.stdin.buffer.readline())
    bot = request["bot"]
    if not re.fullmatch(r"@[A-Za-z0-9_]{5,32}", bot):
        raise ValueError("invalid Telegram bot username")
    text = request["text"]
    speaker = request.get("speaker", "")
    timeout = request["wait_seconds"]
    interval = request["interval_seconds"]
    if not text or len(text) > 8192 or not 1 <= timeout <= 600 or not 0 <= interval <= 600:
        raise ValueError("invalid Telegram speech request")
    api_id = int(os.environ["NM_TELEGRAM_API_ID"])
    api_hash = os.environ["NM_TELEGRAM_API_HASH"]
    session = os.environ["NEUROMITA_TELEGRAM_SESSION"]
    if not api_hash or not session:
        raise ValueError("Telegram is not configured")
    client = TelegramClient(session, api_id, api_hash)
    try:
        await asyncio.wait_for(client.connect(), timeout=30)
        if not await client.is_user_authorized():
            raise RuntimeError("Telegram session requires interactive authorization")
        previous = await client.get_messages(bot, limit=1)
        base_id = previous[0].id if previous else 0
        if speaker:
            if len(speaker) > 256 or "\n" in speaker:
                raise ValueError("invalid Telegram speaker command")
            await client.send_message(bot, speaker)
            await asyncio.sleep(max(0.5, interval))
        if bot == "@CrazyMitaAIbot":
            text = "/voice " + text
        await client.send_message(bot, text)
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            async for message in client.iter_messages(bot, min_id=base_id, limit=8):
                if message.out or not isinstance(message.media, MessageMediaDocument):
                    continue
                document = message.media.document
                mime = (getattr(document, "mime_type", "") or "").lower()
                attributes = getattr(document, "attributes", []) or []
                is_audio = mime == "audio/mpeg" or (
                    mime == "audio/ogg"
                    and any(isinstance(attr, DocumentAttributeAudio) and attr.voice for attr in attributes)
                )
                if not is_audio:
                    continue
                audio = await client.download_media(message, file=bytes)
                if not audio or len(audio) > 20 * 1024 * 1024:
                    raise ValueError("Telegram audio exceeded limit or is empty")
                sys.stdout.buffer.write(base64.b64encode(audio))
                sys.stdout.buffer.flush()
                return
            await asyncio.sleep(0.35)
        raise TimeoutError("Telegram TTS response timed out")
    finally:
        await client.disconnect()


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except Exception as exc:
        sys.stderr.write(type(exc).__name__ + ": " + str(exc)[:240] + "\n")
        sys.exit(1)
