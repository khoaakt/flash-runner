import flash.display.Sprite;
import flash.external.ExternalInterface;

class LirNumbers extends Sprite {
    @:keep public static function identity(a:Float):Float { return a; }
    @:keep public static function negate(a:Float):Float { return -a; }
    @:keep public static function add(a:Float, b:Float):Float { return a + b; }
    @:keep public static function sub(a:Float, b:Float):Float { return a - b; }
    @:keep public static function mul(a:Float, b:Float):Float { return a * b; }
    @:keep public static function div(a:Float, b:Float):Float { return a / b; }
    static function main() { flash.Lib.current.addChild(new LirNumbers()); }
    public function new() {
        super();
        var functions:Array<Dynamic> = [identity, negate, add, sub, mul, div];
        var ok = true, got = 0.0;
        var values = [0.0, 1.0, -1.0, 1.25, -2.5, 3.141592653589793, 65536.0, 0.015625];
        for (i in 0...values.length) for (j in 0...values.length) {
            var a = values[i], b = values[j] == 0 ? 1.5 : values[j];
            var results = [a, -a, a + b, a - b, a * b, a / b];
            for (n in 0...functions.length) {
                var actual = n < 2 ? functions[n](a) : functions[n](a, b);
                if (actual != results[n]) ok = false;
                got += actual;
            }
        }
        graphics.beginFill(ok ? 0x00ff00 : 0xff0000); graphics.drawRect(350, 250, 40, 40); graphics.endFill();
        var text = new flash.text.TextField(); text.width = 500; text.text = (ok ? "PASS" : "FAIL") + ":" + got; addChild(text);
        if (ExternalInterface.available) ExternalInterface.addCallback("selfTest", function() return (ok ? "PASS" : "FAIL") + ":" + got);
    }
}
