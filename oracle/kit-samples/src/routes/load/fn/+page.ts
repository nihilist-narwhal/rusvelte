import { error } from '@sveltejs/kit';

export function load(event) {
	return { slug: event.params.slug };
}
