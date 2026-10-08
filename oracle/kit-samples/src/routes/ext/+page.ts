import { a } from '../../../../shared/a.js';
import type { B } from '../../../../shared/b';
import inside from '../../../lib/inside.js';
import pkg from 'some-package';
export { c } from '../../../../shared/c.js?raw';
export * from "../../../../shared/d.js#hash";
export * as ns from '../../../../shared/ns.js';

type T = import('../../../../shared/t').T;
let x: typeof import('../../../../shared/typeof.js');

/** @type {import('../../../../shared/jsdoc-in-ts').J} */
export const load = async (e) => {
	const m = await import('../../../../shared/dynamic.js');
	const t = await import(`../../../../shared/template.js`);
	const r = require('../../../../shared/required.cjs');
	const notr = (require)('../../../../shared/paren.cjs');
	const v = await import(variable);
	return {};
};
