<?php

declare(strict_types=1);

/** @param list{int} $value */
function take_one(array $value): void {}

/** @param list{int, int} $value */
function take_two(array $value): void {}

/** @param list{int, int} $list */
function the_last_element(array $list): void
{
    unset($list[1]);
    take_one($list);
}

/**
 * @param list{int, int} $list
 *
 * @mago-expect analysis:undefined-int-array-index
 */
function a_missing_index(array $list): void
{
    unset($list[5]);
    take_two($list);
}

/**
 * @param list{int, int} $list
 *
 * @mago-expect analysis:mismatched-array-index
 */
function a_string_key(array $list): void
{
    unset($list['name']);
    take_two($list);
}
