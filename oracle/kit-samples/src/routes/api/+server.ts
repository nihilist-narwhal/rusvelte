import { json } from '@sveltejs/kit';

export async function GET({ url }) {
	return json({ ok: true });
}

export function POST(event) {
	return new Response();
}

export const PUT = async (event) => new Response();
export const PATCH = (event) => {
	return new Response();
};
export const DELETE = async function (e) {
	return new Response();
};
export async function OPTIONS() {}
export function HEAD(e): Response {
	return new Response();
}
export const fallback = async ({ request }) => new Response(request.method);
export const prerender = false;
