import type { Handle } from '@sveltejs/kit';

export const handle = async ({ event, resolve }) => {
	return resolve(event);
};

export function handleError({ error }) {
	return { message: 'oops' };
}

export async function handleFetch({ request, fetch }) {
	return fetch(request);
}

export const transport = {};
export async function init() {}
export const reroute = (e) => '/';
