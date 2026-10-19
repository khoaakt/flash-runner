import flash.display.Sprite;

// Typed getter/setter calls (early-bound method calls, the common AS3 shape) for the A/B in
// tools/bench-flash-lir.mjs-style timing. lir-memory.swf measures Dynamic calls instead.
class LirMemoryBench extends Sprite {
    public var first:Int;
    public var second:Int;
    public function new() { super(); first = 3; second = 5; }
    @:keep public function getFirst():Int { return first; }
    @:keep public function getSum():Int { return first + second; }
    @:keep public function addTo(a:Int):Int { return first + a; }
    @:keep public function setFirst(a:Int):Int { first = a; return first; }
    @:keep public function bump(a:Int):Int { first += a; return first; }
    static function main() {
        var o = new LirMemoryBench();
        flash.Lib.current.addChild(o);
        var repeats = 30, supplied = flash.Lib.current.loaderInfo.parameters.repeats;
        if (supplied != null) { var parsed = Std.parseInt(supplied); if (parsed != null && parsed >= 1 && parsed <= 100) repeats = parsed; }
        var n = repeats * 20000, sum = 0, started = flash.Lib.getTimer();
        for (i in 0...n) {
            o.setFirst(i & 1023);
            sum += o.getFirst() + o.getSum() + o.addTo(i & 7);
            o.bump(1);
        }
        var elapsed = flash.Lib.getTimer() - started;
        // the same arithmetic without calls
        var f = 3, s = 5, direct = 0;
        for (i in 0...n) {
            f = i & 1023;
            direct += f + (f + s) + (f + (i & 7));
            f += 1;
        }
        var ok = sum == direct && o.first == f;
        o.graphics.beginFill(ok ? 0x00ff00 : 0xff0000); o.graphics.drawRect(350, 250, 40, 40); o.graphics.endFill();
        o.graphics.beginFill(elapsed & 0xffffff); o.graphics.drawRect(340, 240, 4, 4); o.graphics.endFill();
    }
}
