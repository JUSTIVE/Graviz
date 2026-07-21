import { Link, useRouterState } from "@tanstack/react-router";
import logo from "@/favicon.png";
import { ThemeToggle } from "./theme-toggle";
import { useSchema } from "@/lib/schema-context";
import { cn } from "@/lib/utils";

declare const __COMMIT_HASH__: string | undefined;
const COMMIT_HASH = typeof __COMMIT_HASH__ === "string" ? __COMMIT_HASH__ : "";

export function AppShell({ children }: { children: React.ReactNode }) {
  const { location } = useRouterState();
  const path = location.pathname;
  const { hasSchema } = useSchema();

  const NAV = [
    { to: "/", label: "New" },
    ...(hasSchema ? [{ to: "/view" as const, label: "View" }] : []),
    { to: "/about", label: "About" },
  ];

  return (
    <div className="flex h-screen w-full flex-col overflow-hidden bg-background text-foreground">
      <header className="sticky top-0 z-20 flex min-h-10 shrink-0 items-center justify-between border-b border-border bg-background/80 px-2 pt-[env(safe-area-inset-top)] backdrop-blur lg:min-h-14 lg:px-4">
        <div className="flex items-center gap-2 lg:gap-6">
          <Link to="/" className="flex items-center gap-1.5 text-sm font-semibold lg:gap-2 lg:text-base">
            <img src={logo} alt="" className="h-4 w-4 lg:h-5 lg:w-5" />
            <span>Graviz</span>
          </Link>
          <nav className="flex items-center gap-0.5 lg:gap-1">
            {NAV.map((item) => {
              const active =
                item.to === "/" ? path === "/" : path.startsWith(item.to);
              return (
                <Link
                  key={item.to}
                  to={item.to}
                  className={cn(
                    "rounded-md px-2 py-1 text-xs transition-colors lg:px-3 lg:py-1.5 lg:text-sm",
                    active
                      ? "bg-secondary text-secondary-foreground"
                      : "text-muted-foreground hover:bg-secondary/60 hover:text-foreground",
                  )}
                >
                  {item.label}
                </Link>
              );
            })}
          </nav>
        </div>
        <div className="flex items-center gap-2">
          <ThemeToggle />
        </div>
      </header>
      <main className="relative flex min-h-0 flex-1 flex-col overflow-hidden">{children}</main>
      {COMMIT_HASH && (
        <div className="pointer-events-none fixed bottom-2 left-1/2 -translate-x-1/2 select-none font-mono text-[10px] text-muted-foreground/40">
          {COMMIT_HASH}
        </div>
      )}
    </div>
  );
}
