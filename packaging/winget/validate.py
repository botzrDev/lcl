"""Checks a release's winget manifests against winget's published schemas.

    python3 -I packaging/winget/validate.py MANIFEST_DIR SCHEMA_DIR

MANIFEST_DIR is the version folder manifests.sh printed. SCHEMA_DIR holds
winget-cli's schemas for the manifest version used, as version.json,
installer.json and defaultLocale.json (from winget-cli's
schemas/JSON/manifests/v1.10.0/). Every file must match its schema, and the
three must agree on the package and version. Exits 1 naming each problem.
Needs PyYAML and jsonschema.
"""

import json
import os
import sys

import jsonschema
import yaml

ID = "botzrDev.lcl"
FILES = {
    "version": f"{ID}.yaml",
    "installer": f"{ID}.installer.yaml",
    "defaultLocale": f"{ID}.locale.en-US.yaml",
}


def main(manifests, schemas):
    problems, seen = [], {}
    for kind, name in FILES.items():
        path = os.path.join(manifests, name)
        try:
            with open(path, encoding="utf-8") as f:
                doc = yaml.safe_load(f)
        except OSError as e:
            problems.append(f"{name}: {e.strerror}")
            continue
        with open(os.path.join(schemas, f"{kind}.json"), encoding="utf-8") as f:
            schema = json.load(f)
        validator = jsonschema.Draft7Validator(schema)
        for err in sorted(validator.iter_errors(doc), key=lambda e: list(e.path)):
            where = "/".join(str(p) for p in err.path) or "(top)"
            problems.append(f"{name}: {where}: {err.message}")
        if doc.get("ManifestType") != kind:
            problems.append(f"{name}: ManifestType is {doc.get('ManifestType')!r}, not {kind!r}")
        seen[name] = (doc.get("PackageIdentifier"), doc.get("PackageVersion"), doc.get("ManifestVersion"))
    if len(set(seen.values())) > 1:
        problems.append(f"the files disagree on (PackageIdentifier, PackageVersion, ManifestVersion): {seen}")
    folder = os.path.basename(os.path.normpath(manifests))
    versions = {v for _, v, _ in seen.values()}
    if versions and versions != {folder}:
        problems.append(f"the folder is {folder!r}, the manifests say {sorted(versions)}")
    for p in problems:
        print(p, file=sys.stderr)
    if problems:
        sys.exit(1)
    print(f"{manifests}: {len(FILES)} manifests match winget's schemas")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit("usage: validate.py MANIFEST_DIR SCHEMA_DIR")
    main(sys.argv[1], sys.argv[2])
