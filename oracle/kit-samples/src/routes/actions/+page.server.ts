import { fail } from '@sveltejs/kit';

export const actions = {
	default: async ({ request }) => {
		const data = await request.formData();
		return fail(400, { data });
	},
	other: async (event) => {}
};
