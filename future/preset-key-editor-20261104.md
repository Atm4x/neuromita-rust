# API key editor correction

The API key field displays the actual resolved key, masked by default, with an
eye toggle and direct editing. Credential references such as `local:python_preset_18`
remain internal and are not shown as the editable key. Imported secrets were not
lost and their files are not rewritten by this UI correction.

Core resolves keys for the editor through a redacted-Debug `SecretKey` read model.
Missing credentials produce an explicit editor error. User-entered keys are saved
to the local credential store; the preset catalog retains references. Saving an
unchanged key keeps its current reference. Replacement creates a fresh entry so
failure to save a preset cannot overwrite the key used by another preset.
Empty keys explicitly select no authentication for supporting protocols; Gemini
rejects that configuration.

The regression test covers reading, replacing, reopening and keeping secret
values out of catalog JSON and Debug output. No live API request is needed.
