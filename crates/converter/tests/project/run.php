<?php

declare(strict_types=1);

namespace Runner;

use Proof\Box;
use Proof\Factory;
use Proof\Shop\Crate;
use Proof\Shop\Feed;
use Proof\Shop\Label;
use Proof\Shop\Ledger;
use Proof\Shop\Note;
use Proof\Shop\Order;
use Proof\Shop\Pair;
use Proof\Shop\Shelf;
use Proof\Shop\Stock;
use Proof\Shop\Tagged;
use Proof\Shop\Tally;
use Proof\Spec;
use Proof\Special;
use Proof\Zoo\Bird;
use Proof\Zoo\Cat;

require __DIR__ . '/vendor/autoload.php';

/** @return \Generator<string, string> */
function keyed(): \Generator
{
    yield 'first' => 'a';
    yield 'second' => 'b';
}

// Calls each class of the fixture from plain PHP in another namespace, before and after the conversion.
$spec = new Spec('alpha');
$special = new Special('beta');
echo $spec->label(), "\n";
echo $special->label(), "\n";
echo $spec->name, "\n";

/** @var Box<Special> $box */
$box = new Box($special);
echo $box->get()->label(), "\n";

/** @var Shelf<Spec> $shelf */
$shelf = new Shelf();
$shelf->add($spec, 7);
$shelf->add($special, 9);
echo $shelf->size(), ' ', count($shelf->all()), ' ', implode(',', array_keys($shelf->numbered())), "\n";
foreach ($shelf->all() as $item) {
    echo $item->label(), "\n";
}
echo $shelf->report(), "\n";

$order = new Order($spec, 'R-1');
$order->add(2);
$order->add(3);
echo $order->summary(), ' ', $order->reference, "\n";
echo $order->isFor('R-1') ? 'for R-1' : 'not for R-1', ' ', $order->isOf($special) ? 'of beta' : 'not of beta', ' ', $order->loud(), "\n";

$crate = new Crate(new Spec('gamma'));
echo $crate->open()->label(), ' ', $crate->special()->get()->label(), "\n";

$tagged = new Tagged('delta', 'new');
echo $tagged->label(), ' ', $tagged->tag, ' ', $tagged->name, "\n";

$note = new Note();
echo $note->describe('hi'), ' / ', $note->describe(5), "\n";

echo (new Factory())->make()->get()->label(), "\n";
echo (new Pair())->both(), "\n";

$tally = new Tally();
$plusThree = $tally->adder(3);
echo $tally->sum(2, 5, 10), ' ', $plusThree(4), ' ', $tally->halved(10), "\n";
echo $tally->counted([3]), ' ', $tally->made(Pair::class), ' ', $tally->priced(['price' => 4, 'count' => 3]), "\n";
echo $tally->carried([3, -1, 4, -2]), "\n";
echo $tally->stamped(new \Acme\Format\Stamp()), "\n";

$label = new Label();
echo $label->render('shelf', 3), "\n", $label->folder(), "\n";
echo $label->status(201), ' ', $label->status(404), ' ', $label->status(500), "\n";

// A PHPDoc `int` that holds `"5"`, a `string` key PHP reads as `int`, and generator keys through a PHP# parameter.
$stock = new Stock();
echo var_export($stock->quantity(new \Acme\Format\Record(['quantity' => '5'])), true), ' ', $stock->joined(keyed()), "\n";
echo (new Ledger())->priceOf(['42' => 1, 'x' => 2], '42'), "\n";
$through = (new Feed())->through(keyed());
foreach ($through as $key => $item) {
    echo var_export($key, true), '=', $item, ' ';
}
echo get_debug_type($through), "\n";

$cat = new Cat('tom');
$bird = new Bird('tweety');
echo $cat->describe(), "\n", $bird->describe(), "\n";
$bird->sound = 'chirp';
$bird->name = 'polly';
echo $bird->describe(), "\n";
