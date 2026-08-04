import { QueryClient } from "@tanstack/react-query";

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 20_000,
      // Thread details carry full HTML (+ CID data-URIs). Keep the default
      // short so hovering the inbox does not leave megabytes of bodies in JS
      // for ten minutes; per-query overrides can still raise this.
      gcTime: 2 * 60_000,
      retry: 1,
      refetchOnWindowFocus: false,
    },
  },
});
