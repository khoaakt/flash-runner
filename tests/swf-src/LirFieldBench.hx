import flash.display.Sprite;

// Typed method calls on an object with Number fields (getters, field arithmetic, a read-modify-write): the shape of
// most AS3 game-object code. A shadow copy of the state, advanced with the same arithmetic, must match exactly.
class LirFieldBench extends Sprite {
    public var px:Float;
    public var py:Float;
    public function new() { super(); px = 1.25; py = -2.5; }
    @:keep public function getX():Float { return px; }
    @:keep public function sumXY():Float { return px + py; }
    @:keep public function scaled(k:Float):Float { return px * k + py; }
    @:keep public function moveBy(dx:Float, dy:Float):Float { px += dx; py += dy; return px; }
    static function main() {
        var o = new LirFieldBench();
        flash.Lib.current.addChild(o);
        var repeats = 30, supplied = flash.Lib.current.loaderInfo.parameters.repeats;
        if (supplied != null) { var parsed = Std.parseInt(supplied); if (parsed != null && parsed >= 1 && parsed <= 100) repeats = parsed; }
        var n = repeats * 10000, viaCalls = 0.0, started = flash.Lib.getTimer();
        for (i in 0...n) { o.moveBy(0.001, 0.002); viaCalls += o.getX() + o.sumXY() + o.scaled(0.5); }
        var elapsed = flash.Lib.getTimer() - started;
        var sx = 1.25, sy = -2.5, direct = 0.0;
        for (i in 0...n) { sx += 0.001; sy += 0.002; direct += sx + (sx + sy) + (sx * 0.5 + sy); }
        var ok = viaCalls == direct && o.px == sx && o.py == sy;
        o.graphics.beginFill(ok ? 0x00ff00 : 0xff0000); o.graphics.drawRect(350, 250, 40, 40); o.graphics.endFill();
        o.graphics.beginFill(elapsed & 0xffffff); o.graphics.drawRect(340, 240, 4, 4); o.graphics.endFill();
    }
}
