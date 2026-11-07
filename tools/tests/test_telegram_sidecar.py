"""Offline contract tests for the Telegram user-session JSON-line sidecar."""
import asyncio
import importlib.util
import json
from pathlib import Path
import sys
import types
import unittest


class PhoneCodeInvalidError(Exception):
    pass


class PhoneCodeExpiredError(Exception):
    pass


class SessionPasswordNeededError(Exception):
    pass


class PasswordHashInvalidError(Exception):
    pass


class FakeClient:
    def __init__(self, session, api_id, api_hash):
        self.connected = False
        self.authorized = False
        self.messages = []
        self.last_id = 0

    async def connect(self):
        self.connected = True

    def is_connected(self):
        return self.connected

    async def disconnect(self):
        self.connected = False

    async def is_user_authorized(self):
        return self.authorized

    async def send_code_request(self, phone):
        return types.SimpleNamespace(phone_code_hash="mock-hash")

    async def sign_in(self, phone=None, code=None, phone_code_hash=None, password=None):
        if password is not None:
            if password != "valid-password":
                raise PasswordHashInvalidError()
            self.authorized = True
            return
        if code == "invalid":
            raise PhoneCodeInvalidError()
        if code == "expired":
            raise PhoneCodeExpiredError()
        if code == "needs-2fa":
            raise SessionPasswordNeededError()
        if code != "valid-code":
            raise PhoneCodeInvalidError()
        self.authorized = True

    async def send_message(self, peer, text):
        self.last_id += 1
        return types.SimpleNamespace(id=self.last_id)

    async def get_messages(self, peer, limit=None, min_id=None, ids=None):
        if ids is not None:
            return next((m for m in self.messages if m.id == ids), None)
        return [m for m in self.messages if m.id > (min_id or 0)][:limit]

    async def download_media(self, message, file=bytes):
        return message.payload


def import_sidecar():
    telethon = types.ModuleType("telethon")
    telethon.TelegramClient = FakeClient
    errors = types.ModuleType("telethon.errors")
    for cls in (PhoneCodeInvalidError, PhoneCodeExpiredError,
                SessionPasswordNeededError, PasswordHashInvalidError):
        setattr(errors, cls.__name__, cls)
    sys.modules["telethon"] = telethon
    sys.modules["telethon.errors"] = errors
    spec = importlib.util.spec_from_file_location(
        "telegram_sidecar_under_test",
        Path(__file__).resolve().parents[1] / "telegram_sidecar.py",
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class TelegramSidecarTest(unittest.TestCase):
    def setUp(self):
        self.sidecar = import_sidecar()

    def call(self, request_id, op, **kwargs):
        return self.sidecar.respond(json.dumps({
            "request_id": request_id, "op": op, **kwargs,
        }).encode())

    def test_invalid_expired_code_and_2fa_retries(self):
        async def scenario():
            s = self.sidecar
            connected = await self.call(1, "connect", api_id=1234, api_hash="fake",
                                        phone="+1000000000", session="/tmp/fake-tg-session")
            self.assertEqual(connected["result"], {"authorized": False})
            self.assertEqual((await self.call(2, "request_code"))["result"],
                             {"requested": True})
            invalid = await self.call(3, "sign_in", code="invalid")
            self.assertEqual(invalid["error"], "invalid_code")
            expired = await self.call(4, "sign_in", code="expired")
            self.assertEqual(expired["error"], "expired_code")
            self.assertEqual((await self.call(5, "sign_in", code="valid-code"))["error"],
                             "code_not_requested")
            await self.call(6, "request_code")
            needed = await self.call(7, "sign_in", code="needs-2fa")
            self.assertTrue(needed["result"]["password_required"])
            self.assertEqual((await self.call(8, "password", password="wrong"))["error"],
                             "invalid_password")
            success = await self.call(9, "password", password="valid-password")
            self.assertTrue(success["result"]["authorized"])
            self.assertEqual((await self.call(10, "status"))["result"],
                             {"authorized": True, "connected": True})
            self.assertEqual((await self.call(11, "disconnect"))["result"],
                             {"disconnected": True})

        asyncio.run(scenario())

    def test_unauthorized_session_and_secret_redaction(self):
        async def scenario():
            await self.call(1, "connect", api_id=1234, api_hash="private-api-hash",
                            phone="+1000000000", session="/tmp/fake-tg-session")
            result = await self.call(2, "send", peer="@silero_voice_bot",
                                     text="sensitive-value")
            self.assertEqual(result["error"], "unauthorized")
            self.assertNotIn("private-api-hash", repr(result))
            self.assertNotIn("sensitive-value", repr(result))

        asyncio.run(scenario())

    def test_reply_metadata_and_download_bounds(self):
        async def scenario():
            s = self.sidecar
            await self.call(1, "connect", api_id=1234, api_hash="fake",
                            phone="+1000000000", session="/tmp/fake-tg-session")
            await self.call(2, "request_code")
            await self.call(3, "sign_in", code="valid-code")
            message = types.SimpleNamespace(
                id=15, out=False, voice=True, audio=False, reply_to_msg_id=12,
                file=types.SimpleNamespace(size=3, mime_type="audio/ogg"),
                payload=b"abc", media=object(),
            )
            s.client.messages = [message]
            recent = await self.call(4, "recent", peer="@silero_voice_bot", after=11)
            self.assertEqual(recent["result"]["messages"][0]["reply_to_id"], 12)
            large = await self.call(5, "download", peer="@silero_voice_bot",
                                    id=15, limit=2)
            self.assertEqual(large["error"], "media_too_large")
            downloaded = await self.call(6, "download", peer="@silero_voice_bot",
                                         id=15, limit=3)
            self.assertEqual(downloaded["result"]["data"], "YWJj")

        asyncio.run(scenario())

    def test_malformed_rpc_does_not_echo_credentials(self):
        async def scenario():
            result = await self.sidecar.respond(
                b'{"request_id": true, "password":"secret-password"}'
            )
            self.assertFalse(result["ok"])
            self.assertNotIn("secret-password", repr(result))
            self.assertIs(result["request_id"], True)

        asyncio.run(scenario())


if __name__ == "__main__":
    unittest.main()
