import flash.display.Sprite;

// Hot loop over nested Number leaves for the alternating A/B in tools/bench-flash-lir.mjs.
// Correctness is mandatory: the summed call results must equal the same expressions evaluated inline.
class LirNumberBench extends Sprite {
    @:keep public static function lerp(a:Float, b:Float, t:Float):Float { return a + (b - a) * t; }
    @:keep public static function distance2(a:Float, b:Float, c:Float, d:Float):Float { return (a - c) * (a - c) + (b - d) * (b - d); }
    @:keep public static function scale(a:Float):Float { return a * 2.5; }
    @:keep public static function offset(a:Float):Float { return a + 1.5; }
    @:keep public static function affine(a:Float, b:Float):Float { return a * 2.0 + b * 0.25; }
    @:keep public static function intToNum(a:Int):Float { return a; }
    @:keep public static function halfInt(a:Int):Float { return a * 0.5; }
    @:keep public static function intPlusNum(a:Int, b:Float):Float { return a + b; }
    @:keep public static function numToInt(a:Float):Int { return Std.int(a); }
    @:keep public static function numToIntScaled(a:Float, b:Float):Int { return Std.int(a * b); }
    @:keep public static function addAC(a:Float, b:Float, c:Float):Float { return a + c; }
    @:keep public static function mulAdd(a:Float, b:Float, c:Float):Float { return a * b + c; }
    static function main() { flash.Lib.current.addChild(new LirNumberBench()); }
    public function new() {
        super();
        var repeats = 30;
        var supplied = flash.Lib.current.loaderInfo.parameters.repeats;
        if (supplied != null) { var parsed = Std.parseInt(supplied); if (parsed != null && parsed >= 1 && parsed <= 100) repeats = parsed; }
        var n = repeats * 10000;
        var viaCalls = 0.0;
        var started = flash.Lib.getTimer();
        for (i in 0...n) {
            var f = i * 0.001;
#if refuse_ops
            // every call has |x| >= 2^31: the replacement refuses each one and the native method (with its helper call) runs
            viaCalls += numToInt(f + 3000000000.0) + numToInt(f - 3000000000.0);
#elseif trunc_ops
            viaCalls += numToInt(f) + numToIntScaled(f, 0.75) + numToIntScaled(f, f);
#elseif conv_ops
            viaCalls += intToNum(i) + halfInt(i) + intPlusNum(i, f);
#elseif const_ops
            viaCalls += scale(f) + offset(f) + affine(f, f + 1.0);
#elseif single_op
            viaCalls += addAC(f, 1.0, 2.0) + addAC(f, 1.0, 3.0) + addAC(f, 1.0, 4.0);
#else
            viaCalls += lerp(f, f + 1.0, 0.5) + distance2(f, f + 1.0, 2.0, 3.0) + mulAdd(f, 1.5, 2.0);
#end
        }
        var elapsed = flash.Lib.getTimer() - started;
        var direct = 0.0;
        for (i in 0...n) {
            var f = i * 0.001;
            var g = f + 1.0;
#if refuse_ops
            direct += Std.int(f + 3000000000.0) + Std.int(f - 3000000000.0);
#elseif trunc_ops
            direct += Std.int(f) + Std.int(f * 0.75) + Std.int(f * f);
#elseif conv_ops
            direct += i + i * 0.5 + (i + f);
#elseif const_ops
            direct += f * 2.5 + (f + 1.5) + (f * 2.0 + (f + 1.0) * 0.25);
#elseif single_op
            direct += (f + 2.0) + (f + 3.0) + (f + 4.0);
#else
            direct += (f + (g - f) * 0.5) + ((f - 2.0) * (f - 2.0) + (g - 3.0) * (g - 3.0)) + (f * 1.5 + 2.0);
#end
        }
        var ok = viaCalls == direct;
        graphics.beginFill(ok ? 0x00ff00 : 0xff0000); graphics.drawRect(350, 250, 40, 40); graphics.endFill();
        graphics.beginFill(elapsed & 0xffffff); graphics.drawRect(340, 240, 4, 4); graphics.endFill();
        var text = new flash.text.TextField(); text.width = 500; text.text = (ok ? "PASS" : "FAIL") + ":" + viaCalls; addChild(text);
    }
}
