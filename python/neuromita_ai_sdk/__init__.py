"""Versioned Python-side contracts for independently installed NeuroMita AI plugins."""

from dataclasses import dataclass, field
from typing import Any, Protocol

API_VERSION = 1


@dataclass(frozen=True)
class PackageRequirement:
    name: str
    version: str


@dataclass(frozen=True)
class AssetRequirement:
    role: str
    relative_path: str
    required: bool = True
    sha256: str | None = None


@dataclass(frozen=True)
class InstallPlan:
    packages: tuple[PackageRequirement, ...] = ()
    assets: tuple[AssetRequirement, ...] = ()
    backend: str | None = None

    def to_wire(self) -> dict[str, Any]:
        return {
            "packages": [vars(item) for item in self.packages],
            "assets": [vars(item) for item in self.assets],
            "backend": self.backend,
        }


class AiPlugin(Protocol):
    def describe(self) -> dict[str, Any]: ...
    def install_plan(self) -> InstallPlan: ...
    def probe_install(self) -> dict[str, Any]: ...
    def settings_schema(self) -> dict[str, Any]: ...
    def initialize(self, settings: dict[str, Any]) -> dict[str, Any]: ...
    def infer(self, input_data: dict[str, Any]) -> dict[str, Any]: ...
    def unload(self) -> None: ...
