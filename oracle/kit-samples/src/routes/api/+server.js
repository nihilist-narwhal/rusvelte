export async function GET({ url }) {
	return new Response();
}
export function POST(event) {
	return new Response();
}
export const PUT = async (event) => new Response();
export const fallback = async function ({ request }) {
	return new Response();
};
