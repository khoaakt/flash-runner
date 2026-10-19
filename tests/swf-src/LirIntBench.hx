import flash.display.Sprite;

// Typed static integer leaf calls in a hot loop, same shape as LirNumberBench. The summed results must
// equal the same arithmetic evaluated inline.
class LirIntBench extends Sprite {
    @:keep public static function mix(a:Int, b:Int):Int { return a * 31 + b; }
    @:keep public static function clamp(a:Int, lo:Int, hi:Int):Int { return a < lo ? lo : (a > hi ? hi : a); }
    @:keep public static function rotate(a:Int, b:Int):Int { return (a << 5) ^ (a >>> 27) ^ b; }
    static function main() { flash.Lib.current.addChild(new LirIntBench()); }
    public function new() {
        super();
        var repeats = 30, supplied = flash.Lib.current.loaderInfo.parameters.repeats;
        if (supplied != null) { var parsed = Std.parseInt(supplied); if (parsed != null && parsed >= 1 && parsed <= 100) repeats = parsed; }
        var n = repeats * 10000, viaCalls = 0, started = flash.Lib.getTimer();
        for (i in 0...n) viaCalls += mix(i, 7) + clamp(i, 100, 5000) + rotate(i, 3);
        var elapsed = flash.Lib.getTimer() - started;
        var direct = 0;
        for (i in 0...n) direct += (i * 31 + 7) + (i < 100 ? 100 : (i > 5000 ? 5000 : i)) + (((i << 5) ^ (i >>> 27)) ^ 3);
        var ok = viaCalls == direct;
        graphics.beginFill(ok ? 0x00ff00 : 0xff0000); graphics.drawRect(350, 250, 40, 40); graphics.endFill();
        graphics.beginFill(elapsed & 0xffffff); graphics.drawRect(340, 240, 4, 4); graphics.endFill();
    }
}
