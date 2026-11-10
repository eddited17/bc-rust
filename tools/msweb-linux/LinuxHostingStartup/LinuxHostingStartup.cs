// ASP.NET Core hosting startup for running Microsoft's BC web server (Prod.Client.WebCoreApp) on Linux
// behind Kestrel, without IIS. Loaded through ASPNETCORE_HOSTINGSTARTUPASSEMBLIES=BcLinux.HostingStartup.
// It does four things:
//
// 1. PathBase. Sets Request.PathBase to ASPNETCORE_APPL_PATH (e.g. "/BC"), which is what IIS + the
//    ASP.NET Core Module do on Windows (IISSetupFilter calls UsePathBase(ASPNETCORE_APPL_PATH)). The web
//    server's RedirectToConsistentApplicationPathCasingRule redirects every request whose PathBase does
//    not equal the application path, so without this every URL redirects to /bc/<path> forever.
//    UseIISIntegration() is a no-op here, because ASPNETCORE_PORT / ASPNETCORE_TOKEN are not set (and its
//    token check would reject browsers that connect directly).
//
// 2. Console logging. On Windows the app logs warnings and errors to the Event Log, which is a no-op on
//    Linux (see ../EventLogStub). Without this, request failures (HTTP 500) leave no trace at all.
//    Minimum level: BC_LINUX_LOG_LEVEL (a Microsoft.Extensions.Logging.LogLevel name), default Warning.
//
// 3. Native shims for four assemblies only, through NativeLibrary.SetDllImportResolver:
//    - Microsoft.Dynamics.Framework.UI.Web: "kernel32.dll" -> libbclinux-kernel32.so (../Kernel32Shim).
//      Its FileHelper.GetFileHash P/Invokes kernel32!CreateFileW and GetFinalPathNameByHandleW.
//    - Microsoft.Dynamics.Framework.UI: "user32.dll" -> libbclinux-user32.so (../User32Shim). Its
//      KeyboardMapper reads the keyboard layout (GetKeyboardLayoutNameW, LoadKeyboardLayoutW, MapVirtualKeyExW,
//      ToUnicodeEx, GetKeyState) to build the shortcut key texts of every action.
//    - System.Private.Windows.Core and System.Private.Windows.GdiPlus (System.Drawing's interop, Windows build):
//      "gdiplus.dll" -> libbclinux-gdiplus.so (../GdiPlusShim), a pass-through for the media thumbnails BC loads
//      with Image.FromStream and saves unchanged. See EnableGdiPlusShim for the one non-standard step.
//    Other assemblies' kernel32/user32 imports, and the other native libraries of these assemblies, keep failing
//    the way they do without this (DllNotFoundException).
//
// 4. Windows time zone ids for the browser time zone detection, see WindowsTimeZones.cs.
using System;
using System.Collections.Generic;
using System.IO;
using System.Reflection;
using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Hosting;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Logging;

[assembly: HostingStartup(typeof(BcLinux.HostingStartup.LinuxHostingStartup))]

namespace BcLinux.HostingStartup
{
    public sealed class LinuxHostingStartup : IHostingStartup
    {
        public void Configure(IWebHostBuilder builder)
        {
            LogLevel level = Enum.TryParse(Environment.GetEnvironmentVariable("BC_LINUX_LOG_LEVEL"), true, out LogLevel parsed)
                ? parsed
                : LogLevel.Warning;
            builder.ConfigureLogging(logging => logging
                .AddSimpleConsole(o => { o.SingleLine = true; o.TimestampFormat = "HH:mm:ss "; })
                .AddFilter<Microsoft.Extensions.Logging.Console.ConsoleLoggerProvider>(null, level));

            MapNativeLibraries("Microsoft.Dynamics.Framework.UI.Web", ("kernel32.dll", "libbclinux-kernel32.so"));
            MapNativeLibraries("Microsoft.Dynamics.Framework.UI", ("user32.dll", "libbclinux-user32.so"));
            MapNativeLibraries("System.Private.Windows.Core", ("gdiplus.dll", "libbclinux-gdiplus.so"));
            EnableGdiPlusShim();
            WindowsTimeZones.ApplyToTimeZoneHelper();

            string? pathBase = Environment.GetEnvironmentVariable("ASPNETCORE_APPL_PATH");
            if (!string.IsNullOrEmpty(pathBase) && pathBase != "/")
            {
                builder.ConfigureServices(services => services.AddSingleton<IStartupFilter>(new PathBaseStartupFilter(pathBase)));
            }
        }

        // System.Drawing's GDI+ imports live in System.Private.Windows.GdiPlus. On anything but Windows, the static
        // constructor of its PInvokeGdiPlus class registers a DllImportResolver for that assembly that always throws
        // PlatformNotSupportedException ("System.Drawing.Common is not supported on non-Windows platforms"), and it
        // fails if another resolver is registered first. So: let the constructor run (it also calls GdiplusStartup,
        // which System.Private.Windows.Core now resolves to the shim), then replace that one entry in the runtime's
        // resolver table, NativeLibrary.s_nativeDllResolveMap (private). GDI+ functions the shim lacks then fail
        // with EntryPointNotFoundException instead of PlatformNotSupportedException.
        private static void EnableGdiPlusShim()
        {
            try
            {
                string shim = Path.Combine(AppContext.BaseDirectory, "libbclinux-gdiplus.so");
                Assembly gdiplus = Assembly.Load("System.Private.Windows.GdiPlus");
                Type pinvoke = gdiplus.GetType("Windows.Win32.PInvokeGdiPlus", throwOnError: true)!;
                RuntimeHelpers.RunClassConstructor(pinvoke.TypeHandle);
                var resolvers = (ConditionalWeakTable<Assembly, DllImportResolver>?)typeof(NativeLibrary)
                    .GetField("s_nativeDllResolveMap", BindingFlags.NonPublic | BindingFlags.Static)?.GetValue(null)
                    ?? throw new MissingFieldException(typeof(NativeLibrary).FullName, "s_nativeDllResolveMap");
                resolvers.AddOrUpdate(gdiplus, (name, _, _) =>
                    name.Equals("gdiplus.dll", StringComparison.OrdinalIgnoreCase) || name.Equals("gdiplus", StringComparison.OrdinalIgnoreCase)
                        ? NativeLibrary.Load(shim)
                        : IntPtr.Zero);
            }
            catch (Exception e)
            {
                Console.Error.WriteLine($"BcLinux.HostingStartup: GDI+ shim not enabled, media thumbnails will fail: {e}");
            }
        }

        // NativeLibrary.SetDllImportResolver can be set once per assembly, so each call maps all of an
        // assembly's shimmed libraries. Names match with or without ".dll", case-insensitively, like on Windows.
        private static void MapNativeLibraries(string assemblyName, params (string Dll, string Shim)[] map)
        {
            var shims = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
            foreach ((string dll, string shim) in map)
            {
                string path = Path.Combine(AppContext.BaseDirectory, shim);
                shims[dll] = path;
                shims[Path.GetFileNameWithoutExtension(dll)] = path;
            }
            NativeLibrary.SetDllImportResolver(Assembly.Load(assemblyName), (name, _, _) =>
                shims.TryGetValue(name, out string? path) ? NativeLibrary.Load(path) : IntPtr.Zero);
        }
    }

    internal sealed class PathBaseStartupFilter : IStartupFilter
    {
        private readonly string pathBase;

        public PathBaseStartupFilter(string pathBase) => this.pathBase = "/" + pathBase.Trim('/');

        public Action<IApplicationBuilder> Configure(Action<IApplicationBuilder> next) => app =>
        {
            app.UsePathBase(pathBase);
            next(app);
        };
    }
}
