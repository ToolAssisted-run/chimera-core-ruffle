using System;
using System.Linq;
using Chimera.Client.Common;
using Chimera.Emulation.Common;
using Chimera.Emulation.Common.Waterbox;

// Does the FRONTEND's own discovery see this package, and does it load?
//
// The engine's parser and the frontend's are different programs reading the
// same file, and the frontend's is the strict one. A package can satisfy the
// gate, chimera-run and every native test while still being unreadable to the
// thing a user actually opens - that is exactly what shipped once, an
// "extensions" declared as a list where the frontend wants a map, which threw
// on load and left the core simply absent from the list with no error in sight.
//
// So this asks the frontend's own CorePackageDiscovery, and nothing else.
class Probe
{
    static int Main(string[] args)
    {
        var found = CorePackageDiscovery.Scan(new[] { args[0] });
        Console.WriteLine($"discovered {found.Count} package(s) in {args[0]}");
        foreach (var p in found.OrderBy(p => p.Name))
        {
            var err = string.IsNullOrEmpty(p.Error) ? "" : "  ERROR: " + p.Error;
            Console.WriteLine($"  {p.Name,-22} {err}");
        }
        var ruffle = found.FirstOrDefault(p => p.Name != null && p.Name.ToLower().Contains("ruffle"));
        if (ruffle == null) { Console.WriteLine("RUFFLE NOT DISCOVERED"); return 1; }
        if (!string.IsNullOrEmpty(ruffle.Error)) { Console.WriteLine("RUFFLE FAILED: " + ruffle.Error); return 1; }
        // The frontend offers a GPU only to a core whose "renderer" setting is
        // named something-hw (WaterboxCore.WantsGpu). This core cannot start
        // without one, so a package that forgets the suffix loads perfectly and
        // then refuses to boot, saying the host offered no GL context.
        // Scan only discovers; load it the way the frontend does to get the factory.
        var loaded = CorePackageLoader.LoadPackage(ruffle.Path);
        var factory = loaded.Factories.OfType<WaterboxCoreFactory>().FirstOrDefault();
        if (factory == null) { Console.WriteLine("RUFFLE HAS NO WATERBOX FACTORY"); return 1; }
        var renderer = factory.Config.Settings?.FirstOrDefault(x => x.Name == "renderer");
        var dflt = renderer?.Default?.ToString() ?? "";
        if (!dflt.EndsWith("-hw", StringComparison.Ordinal))
        {
            Console.WriteLine($"RUFFLE WOULD NOT BE OFFERED A GPU: renderer default is \"{dflt}\"");
            return 1;
        }
        Console.WriteLine($"renderer default \"{dflt}\" asks for a GPU");
        Console.WriteLine("RUFFLE OK");
        return 0;
    }
}
