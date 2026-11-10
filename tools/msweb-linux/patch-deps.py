#!/usr/bin/env python3
"""Register app-local shim assemblies in Prod.Client.WebCoreApp.deps.json.

Usage: python3 -I patch-deps.py <webserver dir>

Some assemblies the BC web server needs (for example Microsoft.Extensions.Logging.EventLog)
ship in the shared framework Microsoft.AspNetCore.App. The .NET host only lets an app-local
copy override a framework assembly if the app's deps.json lists it as a runtime asset with a
higher assemblyVersion, or the same assemblyVersion and a higher fileVersion. This script adds
one synthetic "project" library per shim to the runtime target (the one named in
runtimeTarget) and to "libraries". It is idempotent: running it again rewrites the same entries.
"""
import json
import sys
from pathlib import Path

# name -> (assemblyVersion, fileVersion). The fileVersion must beat the framework copy; .NET 10.0.14
# ships both of these as 10.0.0.0 / 10.0.1426.52326.
SHIMS = {
    # Microsoft.AspNetCore.App; source in EventLogStub/
    "Microsoft.Extensions.Logging.EventLog": ("10.0.0.0", "10.99.0.0"),
    # Microsoft.NETCore.App; source in PrincipalWindowsShim/
    "System.Security.Principal.Windows": ("10.0.0.0", "10.99.0.0"),
    # new app-local assembly (no framework copy); source in LinuxHostingStartup/. It must be in
    # deps.json so ASPNETCORE_HOSTINGSTARTUPASSEMBLIES can load it by name.
    "BcLinux.HostingStartup": ("1.0.0.0", "1.0.0.0"),
}

LIB_PREFIX = "BcLinuxShim."
LIB_VERSION = "1.0.0"


def main() -> None:
    app = Path(sys.argv[1])
    deps_path = app / "Prod.Client.WebCoreApp.deps.json"
    deps = json.loads(deps_path.read_text(encoding="utf-8-sig"))
    target = deps["targets"][deps["runtimeTarget"]["name"]]
    libraries = deps["libraries"]
    for name, (asm_ver, file_ver) in SHIMS.items():
        if not (app / f"{name}.dll").exists():
            sys.exit(f"missing {app / (name + '.dll')}; copy the shim first")
        key = f"{LIB_PREFIX}{name}/{LIB_VERSION}"
        target[key] = {
            "runtime": {f"{name}.dll": {"assemblyVersion": asm_ver, "fileVersion": file_ver}}
        }
        libraries[key] = {"type": "project", "serviceable": False, "sha512": ""}
        print(f"registered {key}")
    deps_path.write_text(json.dumps(deps, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
