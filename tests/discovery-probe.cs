using System;
using System.Linq;
using Chimera.Client.Common;

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
        Console.WriteLine("RUFFLE OK");
        return 0;
    }
}
