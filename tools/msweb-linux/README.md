# Microsoft's BC web server on Linux

This folder runs Microsoft's Business Central web server natively on Linux. That is
`Prod.Client.WebCoreApp` from BC 29.1.57326.57497, an ASP.NET Core app on .NET 10. It runs under Kestrel
with no IIS, no HttpSys and no Windows. It serves the web client at `http://127.0.0.1:8190/BC/` and connects
to a BC service tier, the NST, at `127.0.0.1:7085`. Today that is the real BC container in the Windows VM.
Later it will be bc-rust.

```sh
tools/msweb-linux/run.sh             # builds and installs the shims if needed, then runs in the foreground
tools/msweb-linux/run.sh --install   # rebuilds the shims, copies them again, re-patches deps.json, exits
BC_LINUX_LOG_LEVEL=Information tools/msweb-linux/run.sh   # more console logging, default is Warning
```

To keep it running, start it as a systemd user unit instead of from a shell. `journalctl --user -u
msweb-linux-8190` then shows the log:

```sh
systemd-run --user --unit=msweb-linux-8190 -p Restart=always -p RestartSec=2 \
  --setenv=BC_LINUX_LOG_LEVEL=Information "$PWD/tools/msweb-linux/run.sh"
systemctl --user stop msweb-linux-8190
```

Stop the unit before `run.sh --install`, because the install step overwrites DLLs and libraries that the
running server has loaded.

You need the .NET 10 runtime with ASP.NET Core in `~/.dotnet`, which is 10.0.14 here. You also need the
.NET SDK to build the shims, plus `gcc` and `python3`. `MSWEB_DIR` names the server copy and `MSWEB_PORT`
the port. To set the copy once per machine, put `MSWEB_DIR=<dir>` in `~/.config/bc-rust/msweb.env`; the
environment wins over the file.

## Status

Against the real BC 29.1 service tier, sign-in, the role center and pages 22, 21 and 42 work end to end, in
the browser's own time zone. Icons, customer pictures and action shortcuts all work. The screenshots match the
Windows-hosted web client pixel for pixel, apart from 2 to 15 pixels on two pages. Warm page-open times are the
same as the Windows-hosted web client's, within the noise of this host. Numbers, screenshots and logs are in
`data/capture/linux-webserver-real-nst/run2/notes.md`.

| Request | Result |
|---|---|
| server start | Kestrel listens on 127.0.0.1:8190, about 2 s |
| `GET /`, `/BC`, `/bc/` | 301 to `/BC/` |
| `GET /BC/` | 302 to `/BC/SignIn?ReturnUrl=%2FBC%2F` |
| `GET /BC/SignIn` | 200. The NavUserPassword form with user name, password and antiforgery token. The CSS and the brand image load |
| `POST /BC/SignIn` | The NST validates the credentials over `ws://127.0.0.1:7085/BC/client/ws/connect`, then 302 into the client |
| `/BC/csh` OpenSession + OpenForm | The session opens with the browser's time zone as a Windows id, e.g. `W. Europe Standard Time`. The role center renders |
| `?page=22`, `?page=21`, `?page=42` | Customers, Customer Card 10000 and Sales Order 101001 render with data, factboxes and the customer picture |
| `GET /BC/img?…` | 200, the media thumbnails the NST renders, e.g. `…_70x70.jpg`, `…_360x0.png` |
| `GET /BC/Resources/Fonts/fluentui/*.woff` | 200, the Fluent UI icon fonts |
| `POST /BC/SignIn`, NST down | The server retries for about 76 s, then shows "You cannot sign in due to a technical issue." The log says `NavServerNotFoundException: A server was not found at "http://127.0.0.1:7085/BC/Service"` |
| `GET /BC/Health/System`, NST down | 500. The log says `SystemHealthChecker: Health call failed` with a NavConnectionLostException |

## What run.sh sets

| Setting | Why |
|---|---|
| `cd $MSWEB_DIR` | `Program.Main` reads `hosting.json` and `wwwroot` relative to the current directory. |
| `ASPNETCORE_APPL_PATH=/BC` | If this variable is set, `Program.Main` takes its "hosted inside IIS" branch, which means Kestrel plus `UseIISIntegration()`. Without it the app uses HttpSys, and HttpSys only exists on Windows. The value is also the application path the app enforces, see `AspNetCoreHostingEnvironment.ApplicationPath`. |
| `APP_POOL_CONFIG=applicationHost.config`, `APP_POOL_ID=BC` | The app lowercases `ASPNETCORE_APPL_PATH` to `bc` unless it finds the path in IIS's applicationHost.config, where it takes the casing from. The small stand-in file in this folder keeps `/BC`, so URLs match a Windows install. |
| `ASPNETCORE_HOSTINGSTARTUPASSEMBLIES=BcLinux.HostingStartup` | Loads the hosting startup described below. |
| `ASPNETCORE_URLS=http://127.0.0.1:8190` | The Kestrel endpoint, loopback only. It overrides `http://*:48900` from `hosting.json`. |
| `NAVWebSettings__RequireSsl=false` | navsettings.json has `RequireSsl: "true"`, which makes the antiforgery and auth cookies `Secure`. Over plain HTTP the sign-in page then fails with HTTP 500, `The antiforgery system has the configuration value AntiforgeryOptions.Cookie.SecurePolicy = Always, but the current request is not an SSL request`. Turn it back on behind an HTTPS proxy. |
| `DOTNET_ROOT=~/.dotnet` | Where the runtime lives. |

run.sh leaves WebSocket compression at the navsettings defaults, `EnableWebSocketCompression: true` and
`UseCustomWebSocketCompression: false`. Kestrel then uses permessage-deflate. The Windows BC container sends
gzip+base64 `compressedResult` frames instead. Set `NAVWebSettings__UseCustomWebSocketCompression=true` to get
the same frame shape, for example when comparing /csh captures.

## Every change to the server copy

The copy started as the `WebPublish` folder of the platform artifact, at
`29.1.57326.57497/webclient/WebClient/PFiles/Microsoft Dynamics NAV/290/Web Client/WebPublish`.
Nothing in the original extraction changed.

### navsettings.json, edited before this work

`Server` went from `localhost` to `127.0.0.1`, `ServerInstance` from `""` to `BC`,
`ClientServicesCredentialType` from `Windows` to `NavUserPassword`, `ClientServicesPort` from `"7085"` to
`7085` and `AuthenticateServer` from `"true"` to `false`. `ServerHttps` stays `false`. Whoever edited it also
re-serialized the file, so the indentation changed and the BOM is gone. `RequireSsl` is still `"true"` in
the file. run.sh overrides it.

### Added files

**`Microsoft.Extensions.Logging.EventLog.dll`**, source in `EventLogStub/`. A 40-line no-op logger
provider. `LoggerFactoryConfigurator.ConfigureLoggerFactory` always calls `loggingBuilder.AddEventLog(...)`
unless the app runs in Service Fabric. On Linux the real provider throws
`PlatformNotSupportedException: EventLog access is not supported on this platform` from
`EventLogSettings.CreateDefaultEventLog` as soon as `WebHostBuilder.Build()` creates loggers. No setting
turns it off. Pretending to run in Service Fabric would change far more than the logger.

**`System.Security.Principal.Windows.dll`**, source in `PrincipalWindowsShim/`. Startup crashed in
`ResourcePersistenceManager.VerifyPhysicalApplicationPath` with
`PlatformNotSupportedException: Windows Principal functionality is not supported on this platform`, thrown by
`SafeAccessTokenHandle.get_InvalidHandle`. BC wraps file access in
`WindowsIdentity.RunImpersonated(SafeAccessTokenHandle.InvalidHandle, ...)`, which on Windows means "run as
the process identity". It does this in Framework.UI.WebBase, Nav.Client.WebClient, Nav.Language and
Nav.Watson.

The shim is built from dotnet/runtime source. `PrincipalWindowsShim/fetch-upstream.sh`, which `run.sh --install`
calls, downloads the library's reference source, `Strings.resx` and project file at the .NET release that the
server's deps.json names. That is v10.0.13, and `upstream.lock` pins each file's commit and SHA-256. The script
makes the Linux implementation the way dotnet/runtime's build does for a library whose project sets
`GeneratePlatformNotSupportedAssemblyMessage`: every member throws `PlatformNotSupportedException`
(`gen-notsupported.py`). Then it applies `bc-linux.patch`, which changes two things, both marked `BC-LINUX`.
`SafeAccessTokenHandle` works, and `RunImpersonated` and `RunImpersonatedAsync` call the delegate directly.
The result goes to `PrincipalWindowsShim/gen/`, which git ignores. The first install needs `curl`, `patch` and
access to github.com. For a new BC version whose deps.json names another .NET release, run
`PrincipalWindowsShim/fetch-upstream.sh --update-lock <server dir>` once.

**`BcLinux.HostingStartup.dll`**, source in `LinuxHostingStartup/`. An ASP.NET Core `IHostingStartup`, the
supported way to add code to an app you can't rebuild. It does four things.

1. It calls `UsePathBase(ASPNETCORE_APPL_PATH)`. On Windows, IIS and the ASP.NET Core Module set
   `PathBase=/BC`. Here `UseIISIntegration()` does nothing, because `ASPNETCORE_PORT` and `ASPNETCORE_TOKEN`
   are not set, and Kestrel refuses a path base in its URLs. With an empty PathBase,
   `RedirectToConsistentApplicationPathCasingRule` sends `/BC/` to `/bc/BC/` and keeps redirecting forever.
2. It adds a single-line console logger at `BC_LINUX_LOG_LEVEL`, default Warning, to stand in for the Event
   Log. Before this, an HTTP 500 left no trace at all.
3. It maps Windows DLL names to the C libraries below, for four assemblies only:

   | Assembly | DLL | Library |
   |---|---|---|
   | `Microsoft.Dynamics.Framework.UI.Web` | `kernel32.dll` | `libbclinux-kernel32.so` |
   | `Microsoft.Dynamics.Framework.UI` | `user32.dll` | `libbclinux-user32.so` |
   | `System.Private.Windows.Core` | `gdiplus.dll` | `libbclinux-gdiplus.so` |
   | `System.Private.Windows.GdiPlus` | `gdiplus.dll` | `libbclinux-gdiplus.so` |

   The first three use `NativeLibrary.SetDllImportResolver`. System.Private.Windows.GdiPlus is the GDI+ interop
   of System.Drawing.Common, and the web server ships its Windows build. On anything but Windows, the static
   constructor of its `PInvokeGdiPlus` class registers its own resolver, which always throws
   `PlatformNotSupportedException: System.Drawing.Common is not supported on non-Windows platforms`. It also
   fails if a resolver is already there. So the hosting startup runs that constructor first. The constructor
   calls `GdiplusStartup`, which System.Private.Windows.Core now resolves to the shim. Then the startup swaps
   the throwing resolver for one that loads the shim. The runtime keeps resolvers in the private field
   `NativeLibrary.s_nativeDllResolveMap`, so the swap uses reflection. `Marshal.Prelink` was no way around this,
   because it runs the static constructor before it binds. Every other kernel32, user32 and GDI+ import in the
   app fails as before, with `DllNotFoundException`, or with `EntryPointNotFoundException` for GDI+ functions
   the shim lacks.
4. It makes the browser time zone detection produce Windows time zone ids, described next.

**Windows time zone ids**, source in `LinuxHostingStartup/WindowsTimeZones.cs` and
`LinuxHostingStartup/windows-time-zones.tsv`. `TimeZoneHelper.DetectTimeZone` in Framework.UI.WebBase gets
only offsets from the browser: base offset, DST offset and the DST start and end. It picks the first matching
entry of `TimeZoneInfo.GetSystemTimeZones()` and sends that zone's id to the NST as
`ConnectionRequest.TimeZoneId`. On Linux the ids are IANA ids, and the Windows NST rejected the first one it got:
`NavSCOpenConnectionException: The time zone with ID 'Europe/Amsterdam' was not recognized on the server`.
Setting TZ=UTC for the browser hid this, because `UTC` is a valid id on both systems.

No setting fixes it. .NET builds the list from `$TZDIR/zone.tab`, but `TimeZoneInfo.IdContainsAnyDisallowedChars`
refuses zone ids with a space in them, so a zoneinfo folder with Windows names doesn't load. The
list sits in a private static readonly field, `TimeZoneHelper.TimeZoneCollection`, which reflection cannot set
in .NET 10. The hosting startup therefore swaps the contents of the collection object that field holds, at
startup:

1. It runs TimeZoneHelper's static constructor, so the field holds the runtime's cached collection, then calls
   `TimeZoneInfo.ClearCachedData()`, so that object belongs to TimeZoneHelper alone. The rest of the process
   keeps the normal IANA list.
2. It builds one zone per Windows id. Every installed tzdata file goes through ICU's mapping,
   `TimeZoneInfo.TryConvertIanaIdToWindowsId`, and each Windows id becomes
   `TimeZoneInfo.FindSystemTimeZoneById(windowsId)`. .NET resolves that through ICU to the default IANA zone,
   with the Windows id kept. Ubuntu moved four of ICU's default zones to tzdata-legacy: Asia/Calcutta,
   Asia/Katmandu, Asia/Rangoon and America/Indianapolis. For those it copies the current rules from an installed
   zone with the same Windows id.
3. It sorts the zones the way Windows does, by base offset, then by Windows display name, and keeps only ids
   that Windows has. Both come from `windows-time-zones.tsv`, which is compiled into the DLL. Without the
   Windows names, a browser in New York got "Turks And Caicos Standard Time" and one in India got "Sri Lanka
   Standard Time", because ICU's names sort differently. With them, detection gives the same zone as a
   Windows-hosted web server. That is "W. Europe Standard Time" for Central Europe and "Eastern Standard Time"
   for New York. For Tokyo it is "Transbaikal Standard Time", because "(UTC+09:00) Chita" sorts first on
   Windows too.
4. It points the collection's inner list, the private `list` field of `ReadOnlyCollection<T>`, at the new zones.

The rules and display names of these zones never reach the NST. The NST gets only the id. After OpenSession,
the web `ClientSession`, which has `UseTimeZoneForCurrentSessionOnly = false`, takes over the NST's own
serialized TimeZoneInfo. The startup step costs about 0.3 s. It took 139 zones here, and every one of them
exists in the BC container's list of 144.

To regenerate `windows-time-zones.tsv` from the container, read-only:

```sh
B64=$(printf '%s' '[Console]::OutputEncoding=[Text.Encoding]::UTF8; [TimeZoneInfo]::GetSystemTimeZones() | ForEach-Object { $_.Id + "`t" + $_.DisplayName }' | iconv -t UTF-16LE | base64 -w0)
ssh -i data/vm/bc-vm_ed25519 -p 2222 bcadmin@127.0.0.1 "docker exec bcside powershell -NoProfile -EncodedCommand $B64" | tr -d '\r'
```

Keep the three `#` header lines.

**`libbclinux-kernel32.so`**, source in `Kernel32Shim/kernel32shim.c`. `FileHelper.GetFileHash` computes the
`?h=<sha256>` suffix on CSS, JS and image URLs. For every existing file it hashes, it P/Invokes
`kernel32!CreateFileW` and `GetFinalPathNameByHandleW` to resolve links before it starts a
FileSystemWatcher. That failed with `DllNotFoundException: Unable to load shared library 'kernel32.dll'`.
`/BC/Brand/Images/Header.png` hit it first, and every hashed resource on the client page would too. The shim
implements those two functions with `open(O_PATH)` and `readlink` on `/proc/self/fd/N`. The runtime never
finds it by probing, only through the resolver. So the other kernel32 imports in the app still throw
`DllNotFoundException` as before.

**`libbclinux-user32.so`**, source in `User32Shim/user32shim.c`. Building the role center's actions failed
with `DllNotFoundException: user32.dll` in `KeyboardMapper.GetCurrentKLID`, reached through `ActionBuilder`
and `ShortcutKeysConverter`. `KeyboardMapper` in Framework.UI reads the server's keyboard layout to turn
shortcut keys such as `Ctrl+Alt+N` and `Shift+F9` into text. Its `NativeMethods` imports exactly five
functions. GetKeyboardLayout and ActivateKeyboardLayout are not among them. The shim emulates the Windows US
layout, KLID 00000409, kbdus.dll:

| Function | Result |
|---|---|
| `GetKeyboardLayoutNameW` | `"00000409"` |
| `LoadKeyboardLayoutW` | HKL `0x04090409` for any KLID. Windows falls back to the default layout too |
| `GetKeyState` | 0. No key is down and Caps Lock is off, as in a process without keyboard input |
| `MapVirtualKeyExW` | kbdus's scan code and virtual key tables, all five map types |
| `ToUnicodeEx` | kbdus's characters for no modifier, Shift, Ctrl, Shift+Ctrl and Caps Lock. There are no dead keys and no AltGr |

The results have to be right, not merely successful. `KeyboardMapper.ClearKeyboardBuffer` loops until
`ToUnicodeEx(VK_DECIMAL)` returns 1, and the character map decides how shortcuts print. A Windows web server
uses its IIS worker's layout, which is US on a BC container. I checked the shim against the real
`KeyboardMapper` from the server copy. It reports KLID 00000409, maps `;` to `OemSemicolon`, `\` to `OemPipe`
and `NumPad5` to `5`, prints `Ctrl+Delete` as `Ctrl+Del`, and builds the map in 7 ms.

**`libbclinux-gdiplus.so`**, source in `GdiPlusShim/gdiplusshim.c`. Media thumbnails failed with
`TypeInitializationException: The type initializer for 'Windows.Win32.PInvokeGdiPlus' threw an exception`
and `DllNotFoundException: gdiplus.dll`. These are the customer and item pictures and the role center
images. Every `/BC/img?…` request returned 404, and the log also showed `DirectoryNotFoundException` for
`wwwroot/Thumbnails/default`, which only exists once a thumbnail has been saved. BC calls System.Drawing on
image bytes that the NST already rendered at the right size:

- `LogicalMediaThumbnail.LoadResourceCore` calls `Image.FromStream`.
- The `LogicalImage.Image` setter reads `RawFormat`, then calls `Save(stream, RawFormat)`.
- The `LogicalImage.Image` getter calls `Clone`.
- `ImageControl` and `WebImageHelper` call `RawFormat`.
- `LogicalImage` calls `new Bitmap(stream)`.

None of them touches pixels. The shim is a pass-through for exactly those calls, 14 GDI+ flat-API functions.
It reads the COM `IStream` that System.Drawing passes in, through the stream's vtable. It detects PNG, JPEG,
GIF, BMP, TIFF or ICO by signature and reports the matching `ImageFormat` GUID. It lists GDI+'s five built-in
encoders. When the encoder matches the image's format, it writes the original bytes back. Anything that needs
real decoding fails with a GDI+ status, which System.Drawing turns into an exception that BC's
`ImageResourceCache` logs:

- EMF and WMF
- converting between formats, for example WebImageHelper's .ico to PNG
- drawing and resizing

Windows GDI+ would decode the image and encode it again, giving the same picture in different bytes. The
pass-through serves the NST's bytes unchanged.

### Prod.Client.WebCoreApp.deps.json

`patch-deps.py` adds three libraries of type `project`, named `BcLinuxShim.<name>/1.0.0`. It adds them to the
runtime target `.NETCoreApp,Version=v10.0/win-x64` and to `libraries`, and changes nothing else. The original
is saved as `Prod.Client.WebCoreApp.deps.json.orig`.

`Microsoft.Extensions.Logging.EventLog` and `System.Security.Principal.Windows` belong to the shared
frameworks, Microsoft.AspNetCore.App and Microsoft.NETCore.App. The host only picks an app-local copy over
the framework's if deps.json lists the copy with a higher version. Both shims have assemblyVersion
`10.0.0.0`, the same as the framework, and fileVersion `10.99.0.0`, which beats the framework's
`10.0.1426.52326`. Runtime updates keep 10.0.0.0 and a 10.0.x file version, so the shims should keep winning.

`BcLinux.HostingStartup` has no framework twin. It is in deps.json only so that
`ASPNETCORE_HOSTINGSTARTUPASSEMBLIES` can load it by name.

The three native libraries need no entry. The hosting startup loads them by full path.

### Folders and case aliases under wwwroot

run.sh creates `wwwroot/Resources/ExtractedResources/` and `wwwroot/Thumbnails/`. The Windows installer
normally creates them, and `ResourcePersistenceManager` won't start without them. The error was
`DirectoryNotFoundException: The resources cannot be saved on the web server because folder
…/wwwroot/Resources/ExtractedResources/ does not exist`.

The volume is ntfs3, which is case-sensitive on Linux. Windows is not. Some names are referenced with a
different case than the file on disk has, so run.sh adds an alias with the expected case next to the real
entry. They were found before 2026-11-10 by scanning the server's assemblies and the JS, CSS and HTML under wwwroot for path literals; being re-sourced [src:TODO].

| Alias | Real entry | Referenced by |
|---|---|---|
| `Resources/Brand`, symlink | `Resources/brand` | `BrandProvider.RootDirectory`, used for the sign-in header image |
| `Resources/Images`, symlink | `Resources/images` | `ImageController`, `PathProvider`, `ImagePathProvider`, `AppStoreInfo`, `office.css` |
| `Resources/Fonts`, symlink | `Resources/fonts` | `PathProvider.FluentUIFontPath`, built as `BuildPath("Resources", "Fonts", "fluentui")`, for the Fluent UI icon fonts. A plain literal scan misses it. Without the alias, all icon glyphs were blank |
| `js/boot.js` plus `.br` and `.gz`, hard links | `js/Boot.js` | the `Boot/Script` view, which inlines the file from disk with `ScriptRender.Include` |
| `js/OfficeClientAddIn.js` plus `.br` and `.gz` | `js/OfficeClientAddin.js` | `ScriptDefinitions` |
| `js/file-saver-1.3.3/filesaver.min.js` plus `.br` and `.gz` | `FileSaver.min.js` | `client.js` |

Files get hard links, not symlinks. The static file middleware sends a file symlink's own size as
Content-Length, so `/BC/js/boot.js` came back as 7 bytes, the length of the string "Boot.js".

### Written at runtime

ASP.NET Core keeps its data-protection keys for the antiforgery and auth cookies in
`~/.aspnet/DataProtection-Keys`. That is the Linux default, and the keys are not encrypted at rest. The app
writes extracted resources and thumbnails into the two folders above. Media thumbnails go to
`wwwroot/Thumbnails/<tenant>/`, which is `default` on a single-tenant service tier.

## Undo

Delete the six added files: the three DLLs and the three `libbclinux-*.so`. Then delete the two folders and the
aliases, and put `Prod.Client.WebCoreApp.deps.json.orig` back as `Prod.Client.WebCoreApp.deps.json`.

## Open problems

- Three fixes depend on private names, and an update to .NET or BC can rename them. They are
  `TimeZoneHelper.TimeZoneCollection`, `ReadOnlyCollection<T>.list` and `NativeLibrary.s_nativeDllResolveMap`.
  If one disappears, the hosting startup writes `BcLinux.HostingStartup: ...` to stderr and the server starts
  without that fix. Without the time zone fix, OpenSession fails again unless the browser runs in UTC. Without
  the GDI+ fix, `/BC/img` returns 404 again.
- `windows-time-zones.tsv` comes from the 29.1 container. A newer Windows adds zones. The container already
  has five that ICU does not map: Alberta, British Columbia, Manitoba, Kamchatka and Mid-Atlantic. Regenerate
  the file against the service tier you use. A zone missing from the file can't be detected, but it can't
  break a session either.
- GDI+ covers only bytes in, the same bytes out. Anything that decodes or draws still fails. That covers EMF
  and WMF pictures and .ico pictures shown in an image control, which WebImageHelper converts to PNG. It also
  covers any server-side resize or drawing that a page I didn't open may use.
- The keyboard layout is always US. A Windows web server whose IIS worker has another layout would name
  shortcuts after that layout's keys.
- Tested so far: sign-in, the role center and pages 22, 21 and 42, opened by URL, read-only. Not tested:
  editing and posting, other pages, reports and printing (`winspool.drv`), file upload and download, Excel
  export, Copilot, several users at once, and running behind an HTTPS proxy with `RequireSsl` on. Other
  Windows-only code is still waiting to be hit. The Windows and UserName credential types call `LogonUser`,
  AMSI uses `Amsi.dll`, and performance counters sit behind `EnablePerformanceCounters`.
- URLs that the client builds at runtime can still hit case mismatches. Watch the console log with
  `BC_LINUX_LOG_LEVEL=Information` for 404s and add each one to the alias list in run.sh. The pages above
  produce none.
- When another process killed the server at 13:02 on 2026-11-07, it shut down cleanly with exit code 0. Run it
  as the systemd unit above with `Restart=always` if it has to stay up.
