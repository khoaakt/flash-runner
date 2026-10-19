// Real Flash/NanoJIT integration fixture, not a pre-normalized LIR program.
import flash.display.Sprite;
import flash.external.ExternalInterface;
import flash.text.TextField;

class LirLeaf extends Sprite {
    static function main() { flash.Lib.current.addChild(new LirLeaf()); }
    @:keep static function compare(a:Int, b:Int):Int { return a - b; }
    public function new() {
        super();
        var fn:Dynamic = compare;
        var checksum:Int = 0;
        for (i in 0...200000) checksum += fn(i, i >>> 2);
        var ok = checksum == 2115098112;
        ok = ok && fn(-2147483647 - 1, 1) == 2147483647;
        ok = ok && fn(2147483647, -1) == (-2147483647 - 1);
        var result = (ok ? "PASS" : "FAIL") + ":" + checksum;
        graphics.beginFill(ok ? 0x00ff00 : 0xff0000); graphics.drawRect(350, 250, 40, 40); graphics.endFill();
        var text = new TextField(); text.width = 400; text.text = result; addChild(text);
        if (ExternalInterface.available) ExternalInterface.addCallback("selfTest", function() return result);
    }
}
