import type { PageLoad } from './$types';

export const load: PageLoad = async (e) => ({});
export const prerender: boolean = true;
export const ssr = true satisfies boolean;
export const csr = (false);
export function GET(e: Request) {}
export async function POST(e): Promise<Response> {
	return new Response();
}
export const PUT = ((e) => new Response()) satisfies RequestHandler;
