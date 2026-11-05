"""Rebuild Python DSL reference hashes for the checked-in prompt corpus."""

import argparse
import hashlib
import json
import logging
import os
from pathlib import Path
import sys
import tempfile
import types


def fingerprint(text):
    return hashlib.sha256(text.replace("\r\n", "\n").rstrip().encode("utf-8")).hexdigest()


def main():
    cli = argparse.ArgumentParser()
    cli.add_argument("--python-src", type=Path, required=True)
    cli.add_argument("--prompts", type=Path, required=True)
    cli.add_argument("--output", type=Path, required=True)
    args = cli.parse_args()
    root = args.prompts.resolve()
    args.output.mkdir(parents=True, exist_ok=True)
    sys.path.insert(0, str(args.python_src.resolve()))
    fake = types.ModuleType("main_logger")
    fake.logger = logging.getLogger("prompt_parity_reference")
    sys.modules["main_logger"] = fake
    with tempfile.TemporaryDirectory(prefix="neuromita-dsl-reference-") as working:
        previous = Path.cwd()
        os.chdir(working)
        try:
            from DSL.dsl_engine import DslInterpreter
            from DSL.path_resolver import LocalPathResolver
            from DSL.post_dsl_engine import PostDslInterpreter

            logging.getLogger("dsl_script").disabled = True
            logging.getLogger("dsl_execution").disabled = True

            class Character:
                def __init__(self, char_id, base_data_path, variables):
                    self.char_id = char_id
                    self.base_data_path = str(base_data_path)
                    self.variables = variables
                    self.app_vars = {}
                    self.memory_system = None

                def set_variable(self, name, value):
                    self.variables[name] = value

            class WindowsCompatibleResolver(LocalPathResolver):
                def resolve_path(self, path):
                    candidate = Path(super().resolve_path(path))
                    if candidate.exists():
                        return str(candidate)
                    try:
                        relative = candidate.relative_to(root)
                    except ValueError:
                        return str(candidate)
                    corrected = root
                    for component in relative.parts:
                        exact = corrected / component
                        if exact.exists():
                            corrected = exact
                            continue
                        if not corrected.is_dir():
                            return str(candidate)
                        matches = [entry for entry in corrected.iterdir()
                                   if entry.name.casefold() == component.casefold()]
                        if len(matches) != 1:
                            return str(candidate)
                        corrected = matches[0]
                    return str(corrected)

            reference = {
                "reference": "Python NeuroMita bridge/session-timeline-export-20261101, src/DSL/dsl_engine.py",
                "harness": "Python synthetic Character with set_variable and no memory subsystem; case-insensitive path resolution to emulate Windows; config.json initial values; seed flags disabled",
                "known_python_semantics_differences": {
                    key: "Python implicit None local variable shadows later Character.set_variable attitude_band"
                    for key in [
                        "Cappie/Default", "Cappie/Lite", "Crazy/Lite",
                        "Creepy/Default", "Creepy/Lite", "Ghost/Default",
                        "Ghost/Lite", "Mila/Default", "Mila/Lite",
                    ]
                },
                "sets": {},
            }
            reference["known_python_semantics_differences"]["Kind/LiteJson"] = (
                "Python implicit None local variable shadows Love=0; comparison None>=5 generates DSL ERROR"
            )
            for character in [
                "Crazy", "Kind", "Cappie", "ShortHair", "Mila",
                "Sleepy", "Ghost", "Creepy", "GameMaster",
            ]:
                for template in sorted((root / character).glob("*/main_template.txt")):
                    base = template.parent
                    name = f"{character}/{base.name}"
                    config = (base / "config.json")
                    defaults = json.loads(config.read_text(encoding="utf-8-sig")) if config.exists() else {}
                    vars = {key: value for key, value in defaults.items()
                            if key not in ("custom_params", "metadata")}
                    for param in defaults.get("custom_params", []):
                        if "name" in param and "initial" in param:
                            vars.setdefault(param["name"], param["initial"])
                    for flag in [
                        "SEED_INITIAL_MEMORIES", "SEED_CHARACTER_MEMORIES", "SEED_WORLD_LORE",
                        "SEED_ROOMS_LORE", "SEED_MITAS_LORE", "SEED_ENTITY_LINKS",
                    ]:
                        vars.setdefault(flag, False)
                    vars["PROMPT_SET_NAME"] = base.name
                    person = Character(character, base, vars)
                    engine = DslInterpreter(person, WindowsCompatibleResolver(str(root), str(base)))
                    stable, system = engine.process_main_template("main_template.txt")
                    reference["sets"][name] = {
                        "stable": [fingerprint(text) for text in stable + system],
                        "context": [fingerprint(text) for text in engine.get_context_infos()],
                        "python_error_markers": sum("DSL ERROR" in text or "PY ERROR" in text
                                                    for text in stable + system),
                    }
            target = args.output / "python_reference_sha256.json"
            target.write_text(json.dumps(reference, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

            sample = 'Hello. love_change: +2 <secret_revealed> <action name="Wave" value1="4" value2="true"/> *hello*'
            post = {
                "reference": "Python PostDslInterpreter, sample text and synthetic character state, FIELD rules applied with love_change=2.0/secret_trial=true",
                "text_input": sample,
                "sets": {},
            }
            for script in sorted(root.glob("*/*/PostScripts/main_rules.postscript")):
                if not script.stat().st_size:
                    continue
                base = script.parent.parent
                person = Character(base.parent.name, base, {
                    "Love": 5, "attitude": 60, "boredom": 20, "stress": 10,
                    "secretExposed": False,
                })
                engine = PostDslInterpreter(person, WindowsCompatibleResolver(str(root), str(base)))
                response = engine.process(sample)
                engine.process_structured_fields({"love_change": 2.0, "secret_trial": True})
                post["sets"][base.relative_to(root).as_posix()] = {
                    "text_sha256": fingerprint(response),
                    "variables": person.variables,
                }
            target = args.output / "python_postdsl_reference.json"
            target.write_text(json.dumps(post, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
            print(f"Reference captured: {len(reference['sets'])} Prompt Sets and {len(post['sets'])} PostDSL programs")
        finally:
            os.chdir(previous)


if __name__ == "__main__":
    main()
