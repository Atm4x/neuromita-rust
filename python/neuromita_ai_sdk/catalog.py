"""Bounded wire contracts for dynamic model variants and installation verification."""

from dataclasses import dataclass
import re


@dataclass(frozen=True)
class ModelDescriptor:
    id: str
    label: str
    languages: tuple[str, ...] = ()

    def to_wire(self) -> dict:
        if not isinstance(self.id, str) or not re.fullmatch(r"[A-Za-z0-9_-]{1,80}", self.id):
            raise ValueError("invalid model id")
        if not isinstance(self.label, str) or not self.label.strip() or len(self.label.encode("utf-8")) > 256:
            raise ValueError("invalid model label")
        if not isinstance(self.languages, tuple) or len(self.languages) > 32 or any(
            not isinstance(item, str) or not re.fullmatch(r"[a-z]{2,3}(?:-[A-Za-z0-9]{2,8})?", item)
            for item in self.languages
        ) or len(set(self.languages)) != len(self.languages):
            raise ValueError("invalid model languages")
        return {"id": self.id, "label": self.label, "languages": list(self.languages)}


@dataclass(frozen=True)
class InstallVerification:
    installed: bool
    verified: bool
    problems: tuple[str, ...] = ()

    def to_wire(self) -> dict:
        if type(self.installed) is not bool or type(self.verified) is not bool:
            raise ValueError("installation verification flags must be booleans")
        if self.verified and not self.installed:
            raise ValueError("an absent installation cannot be verified")
        if not isinstance(self.problems, tuple) or len(self.problems) > 64 or any(
            not isinstance(item, str) or not item.strip() or len(item.encode("utf-8")) > 1024
            for item in self.problems
        ):
            raise ValueError("invalid installation verification problems")
        if self.verified and self.problems:
            raise ValueError("verified installation cannot have unresolved problems")
        return {"installed": self.installed, "verified": self.verified, "problems": list(self.problems)}


def encode_models(value: object) -> list[dict]:
    if not isinstance(value, (tuple, list)) or len(value) > 128:
        raise ValueError("list_models must return a bounded list of ModelDescriptor")
    result = []
    seen = set()
    for item in value:
        if not isinstance(item, ModelDescriptor):
            raise ValueError("list_models must return ModelDescriptor instances")
        encoded = item.to_wire()
        if encoded["id"] in seen:
            raise ValueError("duplicate model id")
        seen.add(encoded["id"])
        result.append(encoded)
    return result
