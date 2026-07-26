const { isBig } = require("./calc.ts");

function assertTrue(value: boolean): void {
    if (!value) {
        throw new Error("expected true");
    }
}

function assertFalse(value: boolean): void {
    if (value) {
        throw new Error("expected false");
    }
}

assertFalse(isBig(2));
assertTrue(isBig(3));
