export function reroute({ url }) {
	return url.pathname;
}
export const transport = {
	Vector: { encode: (v) => v, decode: (v) => v }
};
export async function init() {}
