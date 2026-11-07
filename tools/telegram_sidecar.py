#!/usr/bin/env python3
"""Explicit, opt-in Telethon user-session bridge for NeuroMita.

Protocol: newline-delimited UTF-8 JSON request/response on stdin/stdout.
Install Telethon separately, never supply a Bot API token. stderr is for logs.
The host owns starting/stopping this process and must protect its session file.
"""
import asyncio
import base64
import json
import os
import sys

try:
    from telethon import TelegramClient
    from telethon.errors import (
        PhoneCodeInvalidError, PhoneCodeExpiredError,
        SessionPasswordNeededError, PasswordHashInvalidError,
    )
except ImportError:
    print(json.dumps({"ok": False, "error": "telethon_not_installed"}), flush=True)
    sys.exit(2)

client = None
phone = None
phone_code_hash = None
authorized = False


async def handle(req):
    global client, phone, phone_code_hash, authorized
    op = req.get("op")
    if op == "connect":
        if client is not None:
            await client.disconnect()
        api_id = int(req["api_id"])
        api_hash = str(req["api_hash"])
        phone = str(req["phone"])
        session = str(req["session"])
        if not (api_id > 0 and api_hash and phone and session):
            raise ValueError("invalid_credentials")
        if os.name == "posix":
            os.umask(0o077)
        client = TelegramClient(session, api_id, api_hash)
        await asyncio.wait_for(client.connect(), 30)
        authorized = await client.is_user_authorized()
        return {"authorized": authorized}
    if client is None:
        raise RuntimeError("not_connected")
    if op == "request_code":
        result = await client.send_code_request(phone)
        phone_code_hash = result.phone_code_hash
        return {"requested": True}
    if op == "sign_in":
        if not phone_code_hash:
            raise RuntimeError("code_not_requested")
        try:
            await client.sign_in(phone=phone, code=str(req["code"]), phone_code_hash=phone_code_hash)
        except PhoneCodeInvalidError:
            raise RuntimeError("invalid_code") from None
        except PhoneCodeExpiredError:
            phone_code_hash = None
            raise RuntimeError("expired_code") from None
        except SessionPasswordNeededError:
            return {"password_required": True}
        authorized = await client.is_user_authorized()
        return {"authorized": authorized}
    if op == "password":
        try:
            await client.sign_in(password=str(req["password"]))
        except PasswordHashInvalidError:
            raise RuntimeError("invalid_password") from None
        authorized = await client.is_user_authorized()
        return {"authorized": authorized}
    if op == "status":
        authorized = bool(await client.is_user_authorized())
        return {"authorized": authorized, "connected": client.is_connected()}
    if op == "disconnect":
        await client.disconnect()
        authorized = False
        return {"disconnected": True}
    if not authorized or not client.is_connected():
        raise RuntimeError("unauthorized")
    peer = str(req["peer"])
    if op == "latest":
        messages = await client.get_messages(peer, limit=1)
        return {"id": messages[0].id if messages else 0}
    if op == "send":
        message = await client.send_message(peer, str(req["text"]))
        return {"id": message.id}
    if op == "recent":
        after = int(req["after"])
        messages = await client.get_messages(peer, limit=100, min_id=after)
        rows = []
        for msg in messages:
            kind = None
            if msg.voice:
                kind = "voice_ogg"
            elif msg.audio and getattr(msg.file, "mime_type", None) == "audio/mpeg":
                kind = "mp3"
            rows.append({
                "id": msg.id,
                "peer": peer,
                "outgoing": bool(msg.out),
                "reply_to_id": getattr(msg, "reply_to_msg_id", None),
                "kind": kind,
            })
        return {"messages": rows}
    if op == "download":
        message = await client.get_messages(peer, ids=int(req["id"]))
        if message is None or not message.media:
            raise RuntimeError("missing_media")
        size = int(getattr(message.file, "size", 0) or 0)
        limit = int(req["limit"])
        if size > limit:
            raise RuntimeError("media_too_large")
        data = await client.download_media(message, file=bytes)
        if not isinstance(data, bytes) or len(data) > limit:
            raise RuntimeError("media_too_large")
        return {"data": base64.b64encode(data).decode("ascii")}
    raise ValueError("unknown_operation")


async def main():
    while True:
        line = await asyncio.to_thread(sys.stdin.buffer.readline)
        if not line:
            break
        request_id = None
        try:
            req = json.loads(line)
            # Correlation is transport metadata; every normal response echoes it,
            # including invalid auth, network and Telegram errors.
            request_id = req.get("request_id") if isinstance(req, dict) else None
            if not isinstance(request_id, int) or isinstance(request_id, bool) or request_id < 1:
                raise ValueError("invalid_request_id")
            result = await handle(req)
            output = {"ok": True, "request_id": request_id, "result": result}
        except Exception as exc:
            # Never echo credentials, code, password, phone or session paths.
            known = {
                "invalid_credentials", "not_connected", "code_not_requested",
                "invalid_code", "expired_code", "invalid_password",
                "unauthorized", "missing_media", "media_too_large",
                "unknown_operation",
            }
            label = str(exc)
            output = {
                "ok": False, "request_id": request_id,
                "error": label if label in known else "telegram_failure",
            }
        print(json.dumps(output, ensure_ascii=False), flush=True)
    if client is not None:
        await client.disconnect()


if __name__ == "__main__":
    asyncio.run(main())
