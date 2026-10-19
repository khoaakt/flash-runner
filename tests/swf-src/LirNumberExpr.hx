import flash.display.Sprite;
import flash.external.ExternalInterface;

// Finite, ordinary Number expressions only. Signed zero, NaN, denormals and huge magnitudes are
// covered by the Rust differential tests; adding them here stops the movie from painting.
class LirNumberExpr extends Sprite {
    // three-argument single operations: argc is the argv read set, not a capture constant
    @:keep public static function third(a:Float, b:Float, c:Float):Float { return c; }
    @:keep public static function addAC(a:Float, b:Float, c:Float):Float { return a + c; }
    @:keep public static function mulBC(a:Float, b:Float, c:Float):Float { return b * c; }
    // nested SSE arithmetic
    @:keep public static function mulAdd(a:Float, b:Float, c:Float):Float { return a * b + c; }
    @:keep public static function addMul(a:Float, b:Float, c:Float):Float { return (a + b) * c; }
    @:keep public static function subDiv(a:Float, b:Float, c:Float):Float { return (a - b) / c; }
    @:keep public static function lerp(a:Float, b:Float, t:Float):Float { return a + (b - a) * t; }
    @:keep public static function sumSquares(a:Float, b:Float):Float { return a * a + b * b; }
    @:keep public static function diffProduct(a:Float, b:Float):Float { return (a + b) * (a - b); }
    @:keep public static function negMul(a:Float, b:Float):Float { return -(a * b); }
    @:keep public static function sharedSquare(a:Float, b:Float):Float { var s = a + b; return s * s; }
    @:keep public static function distance2(a:Float, b:Float, c:Float, d:Float):Float { return (a - c) * (a - c) + (b - d) * (b - d); }
    // constants (NanoJIT loads them from a static pool slot)
    @:keep public static function scale(a:Float):Float { return a * 2.5; }
    @:keep public static function offset(a:Float):Float { return a + 1.5; }
    @:keep public static function halfSum(a:Float, b:Float):Float { return (a + b) * 0.5; }
    @:keep public static function affine(a:Float, b:Float):Float { return a * 2.0 + b * 0.25; }
    @:keep public static function shiftScale(a:Float, b:Float):Float { return (a - 1.0) * b; }
    @:keep public static function sharedConst(a:Float, b:Float):Float { return a * 3.0 + b * 3.0; }
    // int32 arguments converted exactly, alone and mixed with doubles (packed argv: Int 4 bytes, Float 8 bytes)
    @:keep public static function intToNum(a:Int):Float { return a; }
    @:keep public static function halfInt(a:Int):Float { return a * 0.5; }
    @:keep public static function intPlusNum(a:Int, b:Float):Float { return a + b; }
    @:keep public static function numPlusInt(a:Float, b:Int):Float { return a + b; }
    // Std.int: cvttsd2si with a helper-call fallback the replacement never takes (it refuses |x| >= 2^31)
    @:keep public static function numToInt(a:Float):Int { return Std.int(a); }
    @:keep public static function numToIntScaled(a:Float, b:Float):Int { return Std.int(a * b); }
    static function main() { flash.Lib.current.addChild(new LirNumberExpr()); }
    public function new() {
        super();
        var f:Array<Dynamic> = [third, addAC, mulBC, mulAdd, addMul, subDiv, lerp, sumSquares, diffProduct, negMul, sharedSquare, distance2,
            scale, offset, halfSum, affine, shiftScale, sharedConst];
        var arity = [3, 3, 3, 3, 3, 3, 3, 2, 2, 2, 2, 4, 1, 1, 2, 2, 2, 2];
        var ok = true, got = 0.0;
        var values = [1.0, -1.0, 1.25, -2.5, 3.141592653589793, 65536.0, 0.015625, 7.0];
        for (i in 0...values.length) for (j in 0...values.length) {
            var a = values[i], b = values[j], c = values[(i + j) % values.length], d = values[(i * 3 + j) % values.length];
            if (c == 0) c = 1.5;
            var s = a + b;
            var expected = [c, a + c, b * c, a * b + c, (a + b) * c, (a - b) / c, a + (b - a) * c,
                a * a + b * b, (a + b) * (a - b), -(a * b), s * s, (a - c) * (a - c) + (b - d) * (b - d),
                a * 2.5, a + 1.5, (a + b) * 0.5, a * 2.0 + b * 0.25, (a - 1.0) * b, a * 3.0 + b * 3.0];
            for (n in 0...f.length) {
                var actual:Float = switch (arity[n]) {
                    case 1: f[n](a);
                    case 2: f[n](a, b);
                    case 3: f[n](a, b, c);
                    default: f[n](a, b, c, d);
                };
                if (actual != expected[n]) ok = false;
                got += actual;
            }
        }
        var conv:Array<Dynamic> = [intToNum, halfInt, intPlusNum, numPlusInt];
        var ints = [0, 1, -1, 7, -123456, 65535, 2147483647, -2147483647];
        for (k in ints) for (x in values) {
            var r0:Float = conv[0](k), r1:Float = conv[1](k), r2:Float = conv[2](k, x), r3:Float = conv[3](x, k);
            if (r0 != k || r1 != k * 0.5 || r2 != k + x || r3 != x + k) ok = false;
            got += r0 + r1 + r2 + r3;
        }
        var trunc:Array<Dynamic> = [numToInt, numToIntScaled];
        for (x in values) for (y in values) {
            // 65536 * 65536 is outside int32: that one call stays on the native helper path
            var t0:Int = trunc[0](x), t1:Int = trunc[1](x, y);
            if (t0 != Std.int(x) || t1 != Std.int(x * y)) ok = false;
            got += t0 + t1;
        }
        graphics.beginFill(ok ? 0x00ff00 : 0xff0000); graphics.drawRect(350, 250, 40, 40); graphics.endFill();
        var text = new flash.text.TextField(); text.width = 500; text.text = (ok ? "PASS" : "FAIL") + ":" + got; addChild(text);
        if (ExternalInterface.available) ExternalInterface.addCallback("selfTest", function() return (ok ? "PASS" : "FAIL") + ":" + got);
    }
}
