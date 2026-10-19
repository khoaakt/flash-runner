// Input probe: an INPUT TextField (device font, caret, selection, IME-less typing) and a mirror label of its text.
import flash.display.*;
import flash.events.*;
import flash.text.*;

class Input extends Sprite {
	static function main() { flash.Lib.current.addChild(new Input()); }
	function new() {
		super();
		var field = new TextField();
		field.type = TextFieldType.INPUT; field.border = true; field.background = true; field.backgroundColor = 0xffffcc;
		field.defaultTextFormat = new TextFormat("_sans", 16, 0x000000);
		field.x = 20; field.y = 20; field.width = 300; field.height = 28;
		addChild(field);
		var mirror = new TextField();
		mirror.defaultTextFormat = new TextFormat("_sans", 14, 0x0044aa);
		mirror.x = 20; mirror.y = 70; mirror.width = 440; mirror.height = 80; mirror.multiline = true; mirror.wordWrap = true; mirror.text = "typed: (nothing)";
		addChild(mirror);
		field.addEventListener(Event.CHANGE, function(_) mirror.text = "typed: [" + field.text + "] len=" + field.text.length);
	}
}
