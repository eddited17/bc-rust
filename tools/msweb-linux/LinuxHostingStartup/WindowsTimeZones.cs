// Windows time zone ids for the web server's browser time zone detection.
//
// TimeZoneHelper.DetectTimeZone (Microsoft.Dynamics.Framework.UI.WebBase) takes the browser's UTC and DST
// offsets and picks the first matching entry of a list it captures once, in a static readonly field:
//
//     private static readonly ReadOnlyCollection<TimeZoneInfo> TimeZoneCollection = TimeZoneInfo.GetSystemTimeZones();
//
// The id of the zone it picks goes to the service tier as ConnectionRequest.TimeZoneId. On Windows,
// GetSystemTimeZones returns Windows ids ("W. Europe Standard Time"). On Linux it returns IANA ids
// ("Europe/Amsterdam"), and a Windows service tier rejects those:
// "The time zone with ID 'Europe/Amsterdam' was not recognized on the server".
//
// No setting changes this. .NET reads the list from $TZDIR/zone.tab, but it refuses to load a zone file whose
// id contains a space (TimeZoneInfo.IdContainsAnyDisallowedChars), so a zoneinfo directory with Windows names
// does not work. This class therefore swaps the contents of TimeZoneHelper's private list, once, at startup:
//
// 1. Run TimeZoneHelper's static constructor, so the field holds the runtime's cached collection.
// 2. TimeZoneInfo.ClearCachedData(), so that collection object belongs to TimeZoneHelper alone. The rest of the
//    process keeps getting fresh IANA-id lists from GetSystemTimeZones.
// 3. Point the collection's inner list (ReadOnlyCollection<T>'s private "list" field) at a list of zones with
//    Windows ids, in the order Windows' GetSystemTimeZones returns them.
//
// The Windows zones come from ICU (the same data TimeZoneInfo.FindSystemTimeZoneById uses on Linux to accept a
// Windows id): every tzdata file is mapped to its Windows id, and each Windows id becomes
// FindSystemTimeZoneById(windowsId). That gives the Windows id plus the rules of ICU's default ("001") IANA zone
// for it. Rules and display names don't reach the service tier, only the id does. After OpenSession the web
// client's ClientSession (UseTimeZoneForCurrentSessionOnly = false) adopts the service tier's own TimeZoneInfo.
//
// The order decides which of several zones with the same rules DetectTimeZone picks. Windows sorts by base offset,
// then ordinally by its display names ("(UTC-05:00) Eastern Time (US & Canada)"), which Linux does not have. ICU's
// names ("(UTC-05:00) Eastern Time (Grand Turk)") would give New York browsers "Turks And Caicos Standard Time" and
// Indian ones "Sri Lanka Standard Time". windows-time-zones.tsv (embedded) holds the Windows ids and display names
// of the BC container; zones are sorted by those names and limited to those ids, so detection gives the same zone
// as a Windows-hosted web server (Central Europe: "W. Europe Standard Time") and never an id that Windows lacks.
using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.IO;
using System.Linq;
using System.Reflection;
using System.Runtime.CompilerServices;

namespace BcLinux.HostingStartup
{
    internal static class WindowsTimeZones
    {
        private const string HelperType = "Microsoft.Dynamics.Framework.UI.WebBase.TimeZoneHelper, Microsoft.Dynamics.Framework.UI.WebBase";

        public static void ApplyToTimeZoneHelper()
        {
            try
            {
                Type helper = Type.GetType(HelperType, throwOnError: true)!;
                FieldInfo field = helper.GetField("TimeZoneCollection", BindingFlags.NonPublic | BindingFlags.Static)
                    ?? throw new MissingFieldException(helper.FullName, "TimeZoneCollection");
                FieldInfo inner = typeof(ReadOnlyCollection<TimeZoneInfo>).GetField("list", BindingFlags.NonPublic | BindingFlags.Instance)
                    ?? throw new MissingFieldException(typeof(ReadOnlyCollection<TimeZoneInfo>).FullName, "list");

                RuntimeHelpers.RunClassConstructor(helper.TypeHandle);
                var collection = (ReadOnlyCollection<TimeZoneInfo>)field.GetValue(null)!;
                TimeZoneInfo.ClearCachedData();
                List<TimeZoneInfo> zones = Build();
                TimeZoneInfo.ClearCachedData();   // Build's lookups added Etc/GMT+12 and others to the runtime's own list
                if (zones.Count == 0)
                {
                    Console.Error.WriteLine("BcLinux.HostingStartup: no Windows time zones found (ICU missing?); browser time zone detection keeps IANA ids");
                    return;
                }
                inner.SetValue(collection, zones);
            }
            catch (Exception e)
            {
                Console.Error.WriteLine($"BcLinux.HostingStartup: could not switch TimeZoneHelper to Windows time zone ids: {e}");
            }
        }

        /// <summary>The Windows time zones ICU knows, sorted like TimeZoneInfo.GetSystemTimeZones on Windows.</summary>
        public static List<TimeZoneInfo> Build()
        {
            Dictionary<string, string>? windowsNames = LoadWindowsDisplayNames();
            // Windows id -> IANA ids that map to it, from the tzdata files actually installed.
            var candidates = new SortedDictionary<string, List<string>>(StringComparer.Ordinal);
            foreach (string iana in InstalledIanaIds())
            {
                if (TimeZoneInfo.TryConvertIanaIdToWindowsId(iana, out string? windowsId))
                {
                    if (!candidates.TryGetValue(windowsId, out List<string>? list))
                        candidates[windowsId] = list = new List<string>();
                    list.Add(iana);
                }
            }

            var zones = new List<TimeZoneInfo>(candidates.Count);
            foreach ((string windowsId, List<string> ianaIds) in candidates)
            {
                if (windowsNames != null && !windowsNames.ContainsKey(windowsId))
                    continue;
                TimeZoneInfo? zone = TryFind(windowsId) ?? FromInstalledZone(windowsId, ianaIds);
                if (zone != null && zone.Id == windowsId)
                    zones.Add(zone);
            }
            string SortName(TimeZoneInfo z) => windowsNames != null && windowsNames.TryGetValue(z.Id, out string? name) ? name : z.DisplayName;
            zones.Sort((x, y) =>
            {
                int c = x.BaseUtcOffset.CompareTo(y.BaseUtcOffset);
                return c != 0 ? c : string.CompareOrdinal(SortName(x), SortName(y));
            });
            return zones;
        }

        private static Dictionary<string, string>? LoadWindowsDisplayNames()
        {
            using Stream? stream = typeof(WindowsTimeZones).Assembly.GetManifestResourceStream("windows-time-zones.tsv");
            if (stream == null)
                return null;
            var names = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
            using var reader = new StreamReader(stream);
            for (string? line; (line = reader.ReadLine()) != null;)
            {
                int tab = line.IndexOf('\t');
                if (line.Length > 0 && line[0] != '#' && tab > 0)
                    names[line[..tab]] = line[(tab + 1)..];
            }
            return names.Count > 0 ? names : null;
        }

        // ICU's default IANA zone for this Windows id is a legacy name that the installed tzdata no longer ships
        // (Ubuntu moved Asia/Calcutta, Asia/Katmandu, Asia/Rangoon, America/Indianapolis to tzdata-legacy), so
        // FindSystemTimeZoneById(windowsId) fails. Build the zone from an installed IANA zone that maps to the same
        // Windows id. Only the rules from last year on matter for detecting the browser's zone. CreateCustomTimeZone
        // wants each rule to start after the previous one ends, but .NET's tzdata rules share a boundary day: the
        // rule from the file's POSIX footer starts on the day the last explicit rule (2037) ends. Such a start moves
        // to the next day.
        private static TimeZoneInfo? FromInstalledZone(string windowsId, List<string> ianaIds)
        {
            DateTime from = new DateTime(DateTime.UtcNow.Year - 1, 1, 1);
            foreach (TimeZoneInfo? source in ianaIds.Select(TryFind))
            {
                if (source == null)
                    continue;
                try
                {
                    var rules = new List<TimeZoneInfo.AdjustmentRule>();
                    foreach (TimeZoneInfo.AdjustmentRule rule in source.GetAdjustmentRules().Where(r => r.DateEnd >= from))
                    {
                        TimeZoneInfo.AdjustmentRule r = rule;
                        if (rules.Count > 0 && r.DateStart <= rules[^1].DateEnd)
                        {
                            DateTime start = rules[^1].DateEnd.AddDays(1);
                            if (start > r.DateEnd)
                                continue;
                            r = TimeZoneInfo.AdjustmentRule.CreateAdjustmentRule(start, r.DateEnd, r.DaylightDelta,
                                r.DaylightTransitionStart, r.DaylightTransitionEnd, r.BaseUtcOffsetDelta);
                        }
                        rules.Add(r);
                    }
                    return TimeZoneInfo.CreateCustomTimeZone(windowsId, source.BaseUtcOffset, source.DisplayName, source.StandardName,
                        source.DaylightName, rules.ToArray(), disableDaylightSavingTime: rules.Count == 0);
                }
                catch (Exception e) when (e is ArgumentException || e is InvalidTimeZoneException)
                {
                    Console.Error.WriteLine($"BcLinux.HostingStartup: skipping Windows time zone '{windowsId}' ({source.Id}): {e.Message}");
                }
            }
            return null;
        }

        private static TimeZoneInfo? TryFind(string id)
        {
            try
            {
                return TimeZoneInfo.FindSystemTimeZoneById(id);
            }
            catch (Exception e) when (e is TimeZoneNotFoundException || e is InvalidTimeZoneException)
            {
                return null;
            }
        }

        private static IEnumerable<string> InstalledIanaIds()
        {
            string root = Environment.GetEnvironmentVariable("TZDIR") ?? "/usr/share/zoneinfo";
            if (!Directory.Exists(root))
                yield break;
            foreach (string file in Directory.EnumerateFiles(root, "*", SearchOption.AllDirectories))
            {
                string id = Path.GetRelativePath(root, file);
                if (id.StartsWith("posix/", StringComparison.Ordinal) || id.StartsWith("right/", StringComparison.Ordinal) || id.Contains('.'))
                    continue;   // leap-second and duplicate trees, and zone.tab, tzdata.zi, leap-seconds.list, ...
                yield return id;
            }
        }
    }
}
