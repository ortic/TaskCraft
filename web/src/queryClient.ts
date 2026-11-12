import { QueryClient } from '@tanstack/react-query'

export const queryClient = new QueryClient({
  defaultOptions: { queries: { staleTime: 15_000, refetchOnWindowFocus: false, retry: (n, e: any) => n < 2 && !(e?.status >= 400 && e?.status < 500) } },
})
