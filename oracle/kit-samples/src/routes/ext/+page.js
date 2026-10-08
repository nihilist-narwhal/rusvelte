import { a } from '../../../../shared/a.js';

/**
 * @typedef {import('../../../../shared/typedef.js').T} T
 * @typedef {Object} O
 * @property {import('../../../../shared/prop.js').P} p
 */

/** @type {import('../../../../shared/jsdoc.js').X} */
const x = 1;

/**
 * @param {import('../../../../shared/param.js').E} e
 * @returns {Promise<import("../../../../shared/returns.js").R>}
 */
export async function load(e) {
	/** @type {import('../../../../shared/nested.js').N} */
	const n = await import('../../../../shared/dyn.js');
	const obj = {
		/** @type {import('../../../../shared/prop-assign.js').PA} */
		prop: 1,
		/** @param {import('../../../../shared/method.js').M} m */
		method(m) {}
	};
	foo(/** @type {import('../../../../shared/arg.js').A} */ (n));
	bar(/** @type {import('../../../../shared/not-attached.js').A} */ n);
	return {};
}

/** @type {import('../../../../shared/ssr.js').S} */
export const ssr = true;

/** @satisfies {import('../../../../shared/actions.js').Actions} */
export const actions = {};

class C {
	/** @type {import('../../../../shared/field.js').F} */
	field = 1;
}

/** @type {import('../../../../shared/eof.js').EOF} */
