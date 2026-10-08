interface I {
	/** @type {import('../../../../../shared/iface.js').I} */
	prop: string;
	/** @type {import('../../../../../shared/call-sig.js').C} */
	(): void;
}
enum E {
	/** @type {import('../../../../../shared/enum.js').E} */
	A = 1
}
type Fn = /** @type {import('../../../../../shared/fn-type.js').F} */ (a: number) => void;
type Tup = [/** @type {import('../../../../../shared/tuple.js').T} */ name: string];
import x = require('../../../../../shared/import-equals.js');
const r = require(`../../../../../shared/template-require.js`);
const dyn = import('../../../../../shared/with.js', { with: { type: 'json' } });
declare module '../../../../../shared/declare.js' {}
/** @type {import("../../../../../shared/double.js")} */
"use strict";
export const ssr = true;
