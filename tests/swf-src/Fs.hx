// Fullscreen probe: click toggles Stage.displayState; the label shows the state and stage size.
import flash.display.*;
import flash.events.*;
import flash.text.*;

class Fs extends Sprite {
	var tf:TextField;
	static function main() { flash.Lib.current.addChild(new Fs()); }
	function new() {
		super();
		addEventListener(Event.ADDED_TO_STAGE, function(_) {
			stage.scaleMode = StageScaleMode.NO_SCALE; stage.align = StageAlign.TOP_LEFT;
			tf = new TextField(); tf.defaultTextFormat = new TextFormat("_sans", 14, 0x102030); tf.x = 8; tf.y = 8; tf.width = 380; tf.height = 120; tf.multiline = true; tf.wordWrap = true; addChild(tf);
			stage.addEventListener(MouseEvent.MOUSE_DOWN, function(_) {
				try { stage.displayState = stage.displayState == StageDisplayState.NORMAL ? StageDisplayState.FULL_SCREEN : StageDisplayState.NORMAL; } catch (e:Dynamic) { tf.appendText("ERR " + Std.string(e) + "\n"); }
			});
			stage.addEventListener(FullScreenEvent.FULL_SCREEN, function(e:FullScreenEvent) { tf.appendText("FULL_SCREEN event fs=" + e.fullScreen + "\n"); });
			stage.addEventListener(Event.RESIZE, function(_) show());
			show();
		});
	}
	function show() { tf.appendText("state=" + stage.displayState + " stage=" + stage.stageWidth + "x" + stage.stageHeight + "\n"); }
}
