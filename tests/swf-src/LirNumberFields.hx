import flash.display.Sprite;
import flash.external.ExternalInterface;

// Receiver Number fields: getters, field arithmetic, a Std.int getter, a setter and a read-modify-write method.
// Finite, ordinary values only (edge values live in the Rust differential tests). A shadow copy of the object
// state is updated with the same arithmetic, so a wrong replacement result or a wrong field store paints red.
class LirNumberFields extends Sprite {
    public var px:Float;
    public var py:Float;
    public function new() { super(); px = 1.25; py = -2.5; init(); }
    @:keep public function getX():Float { return px; }
    @:keep public function sumXY():Float { return px + py; }
    @:keep public function dist2():Float { return px * px + py * py; }
    @:keep public function truncX():Int { return Std.int(px); }
    @:keep public function scaled(k:Float):Float { return px * k + py; }
    @:keep public function setX(v:Float):Float { px = v; return v; }
    @:keep public function moveBy(dx:Float, dy:Float):Float { px += dx; py += dy; return px; }
    static function main() { flash.Lib.current.addChild(new LirNumberFields()); }
    function init() {
        var reads:Array<Dynamic> = [getX, sumXY, dist2, truncX];
        var scaledCall:Dynamic = scaled, setCall:Dynamic = setX, moveCall:Dynamic = moveBy;
        var sx = 1.25, sy = -2.5, ok = true, got = 0.0;
        for (i in 0...64) {
            var v = 0.5 + i * 0.25, w = 3.0 - i * 0.125;
            if (reads[0]() != sx || reads[1]() != sx + sy || reads[2]() != sx * sx + sy * sy || reads[3]() != Std.int(sx)) ok = false;
            if (scaledCall(v) != sx * v + sy) ok = false;
            if (setCall(w) != w) ok = false;
            sx = w;
            if (moveCall(v, w) != sx + v) ok = false;
            sx += v; sy += w;
            if (px != sx || py != sy) ok = false;
            got += px + py;
        }
        graphics.beginFill(ok ? 0x00ff00 : 0xff0000); graphics.drawRect(350, 250, 40, 40); graphics.endFill();
        var text = new flash.text.TextField(); text.width = 500; text.text = (ok ? "PASS" : "FAIL") + ":" + got; addChild(text);
        if (ExternalInterface.available) ExternalInterface.addCallback("selfTest", function() return (ok ? "PASS" : "FAIL") + ":" + got);
    }
}
