import type { ParamMatcher } from '@sveltejs/kit';
export const match = ((param: string): param is 'a' => param === 'a') satisfies ParamMatcher;
