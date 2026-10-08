import { error } from '@sveltejs/kit';

/** Loads the page. */
export async function load({ params, fetch }) {
	const res = await fetch(`/api/${params.id}`);
	return { item: await res.json() };
}
