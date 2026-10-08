export const prerender = 'auto' as const;
export const trailingSlash = <const>'always';
export const ssr = !!process.env.SSR;
export const csr = cond ? true : false;
export const actions = { ...base, a: async () => {} } as Actions;
